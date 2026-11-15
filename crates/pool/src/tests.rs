//! Needs the `monty` worker: `DUT_MONTY_WORKER`, else `monty` on PATH, else
//! `~/.cargo/bin/monty`. Skipped when missing, unless `DUT_REQUIRE_WORKER=1` (CI).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use dut_runtime::{FakeHost, Rule, SimDevice};
use eyre::{Result, bail, eyre};

use super::*;

const SENSOR: &str = include_str!("../../../examples/sensor_v2.py");
const STUB: &str = "from dut import Device, write, wait\ndev = Device(\"t\", sdk=\"0.1\")\n";

fn worker() -> Result<Option<PathBuf>> {
    let path_dirs = std::env::var_os("PATH")
        .map(|p| {
            std::env::split_paths(&p)
                .map(|d| d.join("monty"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let found = std::env::var_os("DUT_MONTY_WORKER")
        .map(PathBuf::from)
        .into_iter()
        .chain(path_dirs)
        .chain(std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo/bin/monty")))
        .find(|p| p.is_file());
    if found.is_none() {
        if std::env::var_os("DUT_REQUIRE_WORKER").is_some() {
            bail!("DUT_REQUIRE_WORKER is set but no monty worker binary was found");
        }
        eprintln!(
            "skipping: no monty worker (cargo install monty-runtime --version =1.6.0 --locked)"
        );
    }
    Ok(found)
}

async fn pool(tweak: impl FnOnce(&mut PoolSettings)) -> Result<Option<ScriptPool>> {
    let Some(worker) = worker()? else {
        return Ok(None);
    };
    let mut settings = PoolSettings::new(worker);
    settings.call_timeout = Duration::from_secs(30);
    tweak(&mut settings);
    Ok(Some(ScriptPool::new(settings).await?))
}

fn sensor() -> FakeHost {
    FakeHost::with_device(
        SimDevice::new()
            .at(0.0, b"FW 1.4.2\n")
            .every(1.0, b"T=21.5\n")
            .on(Rule::prefix(b"radio ch ").reply(b"ch=19\n"))
            .on(Rule::prefix(b"radio tx on").reply_at(0.2, b"[radio] TX started\n")),
    )
}

#[tokio::test]
async fn sensor_example_runs_in_a_worker() -> Result<()> {
    let Some(pool) = pool(|_| {}).await? else {
        return Ok(());
    };
    let mut s = pool.open(SENSOR, sensor()).await?;
    assert_eq!(s.registry().map(|r| r.name.as_str()), Some("sensor-v2"));
    s.call_verb("set_channel", &[("channel", Value::Int(19))])
        .await?;
    s.call_verb("tx_on", &[]).await?;
    s.pump(3.0).await?;
    let fw = Value::dict([("fw", "1.4.2".into())]);
    assert_eq!(s.host().of_kind("identity"), [&fw]);
    assert!(s.host().of_kind("record").len() >= 3);
    s.close().await;
    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn sessions_run_in_parallel() -> Result<()> {
    let Some(pool) = pool(|_| {}).await? else {
        return Ok(());
    };
    let pool = Arc::new(pool);
    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let pool = Arc::clone(&pool);
            tokio::spawn(async move {
                let mut s = pool.open(SENSOR, sensor()).await?;
                s.call_verb("tx_on", &[]).await?;
                s.pump(2.0).await?;
                Ok::<_, SessionError>(s.close().await.of_kind("record").len())
            })
        })
        .collect();
    for task in tasks {
        assert!(task.await?? >= 2);
    }
    Ok(())
}

#[tokio::test]
async fn script_errors_keep_the_session() -> Result<()> {
    let Some(pool) = pool(|_| {}).await? else {
        return Ok(());
    };
    let body = format!(
        "{STUB}@dev.verb(\"boom\")\ndef boom():\n    return 1 / 0\n@dev.status\ndef status():\n    return 1\n"
    );
    let mut s = pool.open(&body, FakeHost::new()).await?;
    match s.call_verb("boom", &[]).await {
        Err(SessionError::Script(e)) => assert_eq!(e.exc_type, "ZeroDivisionError"),
        other => bail!("expected a script error, got {other:?}"),
    }
    assert_eq!(s.call_verb("status", &[]).await?, Value::Int(1));
    Ok(())
}

#[tokio::test]
async fn call_deadline_ends_the_session() -> Result<()> {
    let Some(pool) = pool(|_| {}).await? else {
        return Ok(());
    };
    // Each loop waits on the host, which the CPU budget does not count.
    let body = format!(
        "{STUB}@dev.verb(\"stuck\")\ndef stuck():\n    while True:\n        wait(lambda e: False, 1)\n"
    );
    let mut s = pool.open(&body, FakeHost::new()).await?;
    assert_eq!(s.call_verb("stuck", &[]).await, Err(SessionError::Timeout));
    assert_eq!(s.call_verb("stuck", &[]).await, Err(SessionError::Ended));
    Ok(())
}

#[tokio::test]
async fn cpu_runaway_ends_the_session() -> Result<()> {
    let Some(pool) = pool(|s| s.max_cpu_per_call = Duration::from_millis(200)).await? else {
        return Ok(());
    };
    let body = format!("{STUB}@dev.verb(\"spin\")\ndef spin():\n    while True:\n        pass\n");
    let mut s = pool.open(&body, FakeHost::new()).await?;
    assert_eq!(s.call_verb("spin", &[]).await, Err(SessionError::Timeout));
    assert!(!s.is_alive());
    Ok(())
}

#[tokio::test]
async fn memory_limit_ends_only_that_session() -> Result<()> {
    let Some(pool) = pool(|_| {}).await? else {
        return Ok(());
    };
    let body = format!("{STUB}@dev.verb(\"hog\")\ndef hog():\n    return len([0] * 10_000_000)\n");
    let mut hog = pool.open(&body, FakeHost::new()).await?;
    let mut fine = pool.open(SENSOR, sensor()).await?;
    assert_eq!(
        hog.call_verb("hog", &[]).await,
        Err(SessionError::MemoryLimit)
    );
    assert!(!hog.is_alive());
    fine.call_verb("tx_on", &[]).await?;
    pool.open(SENSOR, sensor())
        .await?
        .call_verb("tx_on", &[])
        .await?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_crashed_worker_ends_only_that_session() -> Result<()> {
    let Some(pool) = pool(|_| {}).await? else {
        return Ok(());
    };
    let mut doomed = pool.open(SENSOR, sensor()).await?;
    let mut fine = pool.open(SENSOR, sensor()).await?;
    let pid = doomed.worker_pid().ok_or_else(|| eyre!("no worker pid"))?;
    assert!(
        std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .status()?
            .success()
    );
    match doomed.call_verb("tx_on", &[]).await {
        Err(SessionError::Crashed(_)) => {}
        other => bail!("expected a crash, got {other:?}"),
    }
    fine.call_verb("tx_on", &[]).await?;
    pool.open(SENSOR, sensor())
        .await?
        .call_verb("tx_on", &[])
        .await?;
    Ok(())
}

#[tokio::test]
async fn missing_or_wrong_worker_fails_at_startup() -> Result<()> {
    let missing = ScriptPool::new(PoolSettings::new("/nonexistent/monty")).await;
    assert!(matches!(missing, Err(SessionError::Pool(_))));
    #[cfg(unix)]
    match ScriptPool::new(PoolSettings::new("/usr/bin/true")).await {
        Err(SessionError::Pool(msg)) => assert!(msg.contains("needs 1.6.0"), "{msg}"),
        Err(other) => bail!("expected a version error, got {other:?}"),
        Ok(_) => bail!("a non-Monty worker was accepted"),
    }
    Ok(())
}
