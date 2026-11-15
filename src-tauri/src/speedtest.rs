//! Автоматическое определение скорости тарифа: тест выполняется на самом
//! роутере по SSH (скачивание тестового файла с Cloudflare), поэтому меряется
//! именно WAN-канал, а не Wi-Fi конкретного ПК. Измеренное значение пишется в
//! tariff_mbit и сразу применяется шкалой трея; поле в настройках — только
//! информационное.

use crate::snapshot::AppState;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{AppHandle, Emitter, Manager};

/// защита от параллельных тестов (автотест + кнопка в настройках)
static RUNNING: AtomicBool = AtomicBool::new(false);

/// in-memory backoff после неудачного автотеста (мс от эпохи), чтобы не долбить
/// роутер тестом на каждом опросе при недоступном тестовом сервере
static NEXT_AUTO_ATTEMPT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// порог достоверности: меньше скачали — считаем тест неудачным
const MIN_BYTES: u64 = 5_000_000;

/// Скачиваем тестовый файл на роутере, байты на выходе считает wc -c на нём же —
/// через SSH идёт только итоговое число, а не сам трафик. Один сервер часто
/// сам ограничен по скорости (например, thinkbroadband ~60 Мбит/с на поток),
/// поэтому пробуем несколько и берём МАКСИМАЛЬНЫЙ результат. Первые кандидаты —
/// ipv4-хосты (роутеры без IPv6 не могут dual-stack серверы: uclient-fetch
/// падает с "Operation not permitted"), https — в конце, если есть curl.
const SSH_CMD: &str = r#"TO=""
command -v timeout >/dev/null 2>&1 && TO="timeout 50"
S=$(cut -d' ' -f1 /proc/uptime)
# 4 параллельных потока на ближайший узел anycast-CDN: сумма байтов за общий
# интервал показывает ёмкость канала, а не скорость одного TCP-потока
i=0
while [ $i -lt 4 ]; do
  ( $TO uclient-fetch -q -O - http://cachefly.cachefly.net/100mb.test 2>/dev/null | wc -c > /tmp/.oc_spd_$i ) &
  i=$((i+1))
done
wait
E=$(cut -d' ' -f1 /proc/uptime)
B=$(awk '{s+=$1} END{print s+0}' /tmp/.oc_spd_* 2>/dev/null)
rm -f /tmp/.oc_spd_*
D=$(awk "BEGIN{print $E-$S}")
echo "RESULT bytes=$B dt=$D streams=4"
# CDN недоступен (0 байт) — откат: перебор одиночных серверов по одному,
# берётся максимум, который посчитает принимающая сторона
if [ "$B" -lt 5000000 ]; then
  for c in \
    "uclient-fetch -q -O - http://ipv4.download.thinkbroadband.com/50MB.zip" \
    "uclient-fetch -q -O - http://nbg1-speed.hetzner.de/100MB.bin" \
    "curl -4 -sf --max-time 50 https://speed.cloudflare.com/__down?bytes=100000000"; do
    S=$(cut -d' ' -f1 /proc/uptime)
    B=$($TO $c 2>/dev/null | wc -c)
    E=$(cut -d' ' -f1 /proc/uptime)
    D=$(awk "BEGIN{print $E-$S}")
    echo "RESULT bytes=$B dt=$D"
  done
fi
"#;

/// Достаём все строки «RESULT bytes=N dt=X.Y» — вернём их списком.
fn parse_results(out: &[u8]) -> Vec<(u64, f64)> {
    String::from_utf8_lossy(out)
        .lines()
        .filter(|l| l.contains("RESULT") && l.contains("bytes="))
        .filter_map(|l| {
            let bytes = l
                .splitn(2, "bytes=")
                .nth(1)?
                .split_whitespace()
                .next()?
                .parse()
                .ok()?;
            let dt = l
                .splitn(2, "dt=")
                .nth(1)?
                .split_whitespace()
                .next()?
                .parse()
                .ok()?;
            Some((bytes, dt))
        })
        .collect()
}

/// Округление до «тарифных» значений, достаточно мелкие шаги, чтобы
/// 200-мегабитный план не занижался до 150: 2/5/10/25 Мбит/с по диапазонам.
fn round_mbit(v: f64) -> u32 {
    let r = if v < 20.0 {
        (v / 2.0).round() * 2.0
    } else if v < 100.0 {
        (v / 5.0).round() * 5.0
    } else if v < 200.0 {
        (v / 10.0).round() * 10.0
    } else {
        (v / 25.0).round() * 25.0
    };
    (r.max(1.0)) as u32
}

/// Запустить тест и сохранить результат. Возвращает измеренную скорость
/// в Мбит/с. Ошибку возвращает как строку (для вывода в настройках).
/// Если параллельно идёт автотест — ждём его завершения (кнопка в настройках
/// показывает спиннер), а не отваливаемся с ошибкой.
pub async fn measure_and_save(state: &tauri::State<'_, AppState>) -> Result<u32, String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
    while RUNNING.swap(true, Ordering::SeqCst) {
        // флаг уже поднят автотестом: swap оставил его как есть, просто ждём
        if std::time::Instant::now() > deadline {
            return Err("Тест уже выполняется (автоматически) — попробуйте через минуту".into());
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    let res = run_once(state).await;
    RUNNING.store(false, Ordering::SeqCst);
    res
}

async fn run_once(state: &tauri::State<'_, AppState>) -> Result<u32, String> {
    let (host, user, pass) = {
        let s = state.settings.inner.read();
        (s.host.clone(), s.username.clone(), s.password())
    };
    if host.is_empty() {
        return Err("Роутер не настроен".into());
    }

    let started = std::time::Instant::now();
    // долгий вызов: скачивание молчит до конца, 60-секундный per-chunk таймаут
    // пула обрывал бы даже успешный тест
    let out = crate::ssh::run_slow_command(&host, &user, &pass, SSH_CMD).await?;
    let total = started.elapsed().as_secs_f64();
    // берём максимум по всем серверам: один сервер часто медленнее канала
    let best = parse_results(&out)
        .iter()
        .filter(|&&(b, d)| b >= MIN_BYTES && d >= 0.2)
        .map(|&(b, d)| b as f64 * 8.0 / d / 1_000_000.0)
        .fold(0.0f64, f64::max);
    if best <= 0.0 {
        return Err(format!(
            "Не удалось скачать тестовый файл (вся команда {total:.1}с). \
Проверьте интернет на роутере и наличие uclient-fetch/curl/wget. Вывод роутера: {}",
            String::from_utf8_lossy(&out).trim().chars().take(1500).collect::<String>()
        ));
    }
    let mbit = round_mbit(best);

    // применяем всегда: пишем значение + время теста и сохраняем конфиг
    {
        let mut s = state.settings.inner.write();
        s.tariff_mbit = mbit;
        s.tariff_test_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        state.settings.save(&s).map_err(|e| e.to_string())?;
    }
    Ok(mbit)
}

/// Автотест по расписанию: вызывается из цикла опроса после каждого
/// успешного снапшота. Условия: интервал != «никогда», прошло больше
/// интервала с последнего теста (или его ещё не было), нет backoff.
pub fn maybe_autotest(app: &AppHandle) {
    let (days, test_at) = {
        let state = app.state::<AppState>();
        let s = state.settings.inner.read();
        (s.tariff_test_days, s.tariff_test_at)
    };
    if days == 0 {
        return; // «никогда» — только вручную
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let interval_ms = (days as i64) * 86_400_000;
    if test_at + interval_ms > now {
        return; // срок ещё не наступил
    }
    if NEXT_AUTO_ATTEMPT.load(Ordering::SeqCst) > now {
        return; // недавно уже неудачно пробовали
    }
    // следующий автопроб не раньше чем через час, даже если тест упадёт
    NEXT_AUTO_ATTEMPT.store(now + 3_600_000, Ordering::SeqCst);

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let started = std::time::Instant::now();
        if RUNNING.swap(true, Ordering::SeqCst) {
            return; // ручной тест уже идёт
        }
        let res = run_once(&state).await;
        RUNNING.store(false, Ordering::SeqCst);
        match res {
            Ok(mbit) => {
                let _ = app.emit("tariff-detected", serde_json::json!({ "mbit": mbit }));
            }
            Err(e) => {
                eprintln!("speedtest: автотест не удался за {:?}: {e}", started.elapsed());
            }
        }
    });
}
