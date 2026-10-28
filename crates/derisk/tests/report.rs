use std::time::{Duration, Instant};

use derisk::{
    action::{Action, Effect},
    geom::rect,
    palette,
    report::{self, Event, Problem, Report, Reporter, Reporting},
    shell::{Error, Shell},
};
use serde_json::json;

fn shell() -> Shell {
    Shell::new(rect(0, 0, 1920, 1080), false)
}

/// A reporter as `sh -c script`.
fn sh(script: &str) -> Vec<String> {
    ["sh", "-c", script].map(str::to_owned).to_vec()
}

#[test]
fn problems_ask_the_reporter_in_its_words() {
    // The fields losos-report's `Problem::parse` reads.
    let hung = Problem::NotResponding {
        app: "org.example.Viewer".into(),
        pid: Some(42),
        seconds: 7,
    };
    assert_eq!(
        hung.request(),
        json!({"request": "report", "kind": "not_responding", "app": "org.example.Viewer", "pid": 42, "seconds": 7})
    );
    let failed = Problem::Failed {
        unit: "app-derisk-org.foo@3.service".into(),
        user: true,
    };
    assert_eq!(
        failed.request(),
        json!({"request": "report", "kind": "failed", "unit": "app-derisk-org.foo@3.service", "user": true})
    );
    let stuck = Problem::Stuck {
        app: "org.derisk.settings".into(),
        action: "Pairing…".into(),
        seconds: 31,
        pid: None,
    };
    assert_eq!(
        stuck.request(),
        json!({"request": "report", "kind": "stuck", "app": "org.derisk.settings", "action": "Pairing…", "seconds": 31, "pid": null})
    );
    assert_eq!(hung.summary("Viewer"), "Viewer stopped responding");
    assert_eq!(failed.summary(""), "app-derisk-org.foo@3.service failed");
    assert_eq!(
        stuck.summary("Settings"),
        "Settings: \u{201c}Pairing…\u{201d} never finished"
    );
}

#[test]
fn a_report_is_written_then_signed_in_for_then_sent() {
    let mut report = Report::default();
    assert!(!report.can_send());
    report.apply(Event::Report {
        repository: "losos-project/desktop".into(),
        title: "Viewer stopped responding [ec62b943]".into(),
        body: "…".into(),
    });
    assert!(report.can_send());
    assert!(!report.sending());
    report.state = Reporting::Starting;
    assert!(report.sending() && !report.can_send());
    report.apply(Event::ReportSignIn {
        uri: "https://github.com/login/device".into(),
        code: "ABCD-1234".into(),
    });
    assert!(report.sending());
    report.apply(Event::ReportFailed {
        message: "GitHub could not be reached.".into(),
    });
    // A failed send can be tried again: the text is still there.
    assert!(report.can_send());
    report.apply(Event::Reported {
        url: "https://github.com/losos-project/desktop/issues/1".into(),
        existing: true,
    });
    assert!(report.sent() && !report.can_send());
}

#[test]
fn a_reporter_writes_and_sends_through_its_program() {
    // Answers the way losos-report does, and fails the test (by staying
    // silent) if the requests are not the ones it expects.
    let script = r#"
echo '{"event":"hello","report":"losos-project/desktop"}'
read -r line
case "$line" in *'"kind":"failed"'*) ;; *) exit 1 ;; esac
case "$line" in *'"unit":"foo.service"'*) ;; *) exit 1 ;; esac
echo '{"event":"report","repository":"losos-project/desktop","title":"foo.service failed","body":"text"}'
read -r line
case "$line" in *'"request":"report_send"'*) ;; *) exit 1 ;; esac
case "$line" in *'"note":"it broke"'*) ;; *) exit 1 ;; esac
echo '{"event":"report_sign_in","uri":"https://github.com/login/device","code":"ABCD-1234"}'
echo '{"event":"reported","url":"https://github.com/losos-project/desktop/issues/9","existing":false}'
read -r line
"#;
    let problem = Problem::Failed {
        unit: "foo.service".into(),
        user: true,
    };
    let mut reporter = Reporter::start(&sh(script), &problem).unwrap();
    let wait = |reporter: &mut Reporter, done: fn(&Report) -> bool| {
        let start = Instant::now();
        while !done(&reporter.report) {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "the reporter went quiet"
            );
            std::thread::sleep(Duration::from_millis(10));
            reporter.poll();
        }
    };
    wait(&mut reporter, Report::can_send);
    let (_, title, _) = reporter.report.written.clone().unwrap();
    assert_eq!(title, "foo.service failed");
    reporter.report.note = "it broke".into();
    reporter.send();
    wait(&mut reporter, Report::sent);
    let Reporting::Sent { url, existing, .. } = &reporter.report.state else {
        unreachable!()
    };
    assert_eq!(url, "https://github.com/losos-project/desktop/issues/9");
    assert!(!existing);
}

#[test]
fn reports_are_offered_only_where_a_reporter_says_where_they_go() {
    assert_eq!(
        report::probe(&sh(r#"echo '{"event":"hello","report":"o/n"}'; read -r _"#)),
        Some("o/n".into())
    );
    assert_eq!(
        report::probe(&sh(r#"echo '{"event":"hello","report":null}'; read -r _"#)),
        None
    );
    // Off LosOS there is no reporter at all.
    assert_eq!(
        report::probe(&["derisk-test-no-such-reporter".to_owned()]),
        None
    );
}

#[test]
fn a_failed_unit_is_reported_only_while_it_is_failed() {
    let mut shell = shell();
    let report = |unit: &str| Action::ReportUnit { unit: unit.into() };
    assert!(matches!(
        shell.apply(report("foo.service")),
        Err(Error::UnknownUnit(_))
    ));
    shell.failed_units = vec!["foo.service".into()];
    assert_eq!(
        shell.apply(report("foo.service")).unwrap(),
        vec![Effect::Report {
            problem: Problem::Failed {
                unit: "foo.service".into(),
                user: true,
            },
            window: None,
        }]
    );
}

#[test]
fn a_window_is_reported_with_its_app_and_left_for_the_host_to_fill_in() {
    let mut shell = shell();
    let (window, _) = shell.map_window("org.example.Viewer", "secret plans.odt");
    let effects = shell
        .apply(Action::ReportWindow {
            window: Some(window.get()),
        })
        .unwrap();
    // The title, the person's document, is not in it.
    assert_eq!(
        effects,
        vec![Effect::Report {
            problem: Problem::NotResponding {
                app: "org.example.Viewer".into(),
                pid: None,
                seconds: 0,
            },
            window: Some(window.get()),
        }]
    );
}

#[test]
fn the_palette_offers_to_report_a_failed_service_only_where_reports_go() {
    let mut shell = shell();
    shell.failed_units = vec!["foo.service".into()];
    let titles = |shell: &Shell| -> Vec<String> {
        palette::entries(shell, &[], &[])
            .into_iter()
            .filter(|e| e.detail == "Failed service")
            .map(|e| e.title)
            .collect()
    };
    assert_eq!(
        titles(&shell),
        ["Restart foo.service", "Dismiss foo.service"]
    );
    shell.can_report = true;
    assert_eq!(
        titles(&shell),
        [
            "Restart foo.service",
            "Dismiss foo.service",
            "Report foo.service"
        ]
    );
    let report = palette::entries(&shell, &[], &[])
        .into_iter()
        .find(|e| e.title == "Report foo.service")
        .unwrap();
    assert!(report.reports());
    assert_eq!(
        report.actions,
        [Action::ReportUnit {
            unit: "foo.service".into()
        }]
    );
}
