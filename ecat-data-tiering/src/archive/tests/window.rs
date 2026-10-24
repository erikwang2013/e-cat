// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 运行时间窗：UTC `HH:MM-HH:MM`（含跨午夜）。

use super::*;

fn hhmm(minute: u32) -> String {
    format!("{:02}:{:02}", (minute / 60) % 24, minute % 60)
}

fn now_minute() -> u32 {
    let now = OffsetDateTime::now_utc();
    u32::from(now.hour()) * 60 + u32::from(now.minute())
}

#[tokio::test]
async fn archive_skips_outside_window() {
    let mut config = config(1);
    let minute = now_minute();
    config.window = Some(format!("{}-{}", hhmm(minute + 120), hhmm(minute + 180)));
    let cold = FakeCold::new(true);
    let (job, state) = job(
        config,
        FakeHot::new(&["p2026_01"], vec![event_row(1)]),
        cold.clone(),
        None,
    )
    .await;

    let report = job.run_once().await.unwrap();

    assert!(report.skipped);
    assert_eq!(cold.write_count(), 0);
    assert!(state.load("events", "p2026_01", 0).await.unwrap().is_none());
}

#[tokio::test]
async fn archive_runs_inside_window() {
    let mut config = config(1);
    let minute = now_minute();
    config.window = Some(format!("{}-{}", hhmm(minute + 1435), hhmm(minute + 5)));
    let cold = FakeCold::new(true);
    let (job, _) = job(
        config,
        FakeHot::new(&["p2026_01"], vec![event_row(1)]),
        cold.clone(),
        None,
    )
    .await;

    let report = job.run_once().await.unwrap();

    assert!(!report.skipped);
    assert_eq!(cold.write_count(), 1);
}
