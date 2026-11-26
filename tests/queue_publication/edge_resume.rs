use super::auto_resume::Running;
use super::common::git::git_in;
use super::count;
use super::fixture::{Fixture, REVIEW_URL};
use std::fs;
use std::time::Duration;

fn selected_edge() -> Fixture {
    let fixture = Fixture::new(true, true);
    let path = fixture.dir.join("pipetrain.toml");
    let cfg = fs::read_to_string(&path).unwrap();
    fs::write(
        path,
        cfg.replace("package_max_behind = 1", "package_max_behind = 20"),
    )
    .unwrap();
    let checkout = fixture.dir.join("web");
    git_in(&checkout, &["switch", "feature/2"]);
    git_in(&checkout, &["rm", "Feature2.cs"]);
    let project = checkout.join("Web.csproj");
    let contents = fs::read_to_string(&project).unwrap();
    fs::write(project, contents.replace("1.0.0", "1.0.1")).unwrap();
    git_in(&checkout, &["add", "Web.csproj"]);
    git_in(
        &checkout,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            "Reference the unpublished package",
        ],
    );
    {
        let mut gh = fixture.gh.lock().unwrap();
        git_in(&checkout, &["push", gh.repo.to_str().unwrap(), "feature/2"]);
        gh.changed_pin = true;
    }
    fixture
}

#[test]
#[ignore = "requires .NET SDK 10 and the net8.0 targeting pack"]
fn one_merge_process_prepares_an_edge_review_and_resumes_its_verified_consumer() {
    let fixture = selected_edge();
    let mut run = Running::launch(
        &fixture,
        fixture.selected_command(
            &["acme/sdk#1", "acme/web#2", "acme/other#4"],
            &["--apply", "--yes", "--json", "--timeout-minutes", "1"],
        ),
    );
    run.until("consumer branch draft and independent merge", |gh| {
        gh.reviews.len() == 1 && gh.merged.contains(&4)
    });
    let (prepared, start) = {
        let mut gh = fixture.gh.lock().unwrap();
        let review = &mut gh.reviews[0];
        assert_eq!(review["base"]["ref"], "feature/2");
        assert_eq!(review["html_url"], REVIEW_URL);
        review["draft"] = false.into();
        review["reviewDecision"] = "APPROVED".into();
        (
            review["head"]["sha"].as_str().unwrap().to_owned(),
            gh.review_reads.len(),
        )
    };
    run.until(
        "two polls of the approved, open consumer branch bump",
        |gh| gh.review_reads.len() >= start + 2,
    );
    {
        let mut gh = fixture.gh.lock().unwrap();
        assert!(!gh.merged.contains(&2));
        assert_ne!(gh.head(2), prepared);
        gh.integrate();
        assert_eq!(gh.head(2), prepared);
        gh.readiness
            .insert(prepared.clone(), ("PENDING", "REVIEW_REQUIRED"));
    }
    run.until(
        "pending verification of the incorporated consumer head",
        |gh| {
            gh.seen_readiness
                .contains(&(2, prepared.clone(), "PENDING", "REVIEW_REQUIRED"))
        },
    );
    {
        let mut gh = fixture.gh.lock().unwrap();
        assert!(!gh.merged.contains(&2));
        gh.readiness
            .insert(prepared.clone(), ("PENDING", "APPROVED"));
    }
    run.until(
        "approval with pending CI on the incorporated consumer",
        |gh| {
            gh.seen_readiness
                .contains(&(2, prepared.clone(), "PENDING", "APPROVED"))
        },
    );
    {
        let mut gh = fixture.gh.lock().unwrap();
        assert!(!gh.merged.contains(&2));
        gh.readiness
            .insert(prepared.clone(), ("SUCCESS", "APPROVED"));
    }
    let report = run.finish(0, Duration::from_secs(45));
    let gh = fixture.gh.lock().unwrap();
    assert_eq!(report["summary"]["merged"], 3, "{report}");
    assert_eq!(gh.reviews.len(), 1);
    assert_eq!(count(&gh.writes, "POST /repos/acme/web/git/refs"), 1);
    assert_eq!(count(&gh.writes, "POST /repos/acme/web/pulls"), 1);
    for target in ["sdk/pulls/1", "web/pulls/2", "other/pulls/4"] {
        assert_eq!(
            count(&gh.writes, &format!("PUT /repos/acme/{target}/merge")),
            1
        );
    }
    assert_eq!(gh.writes.len(), 5, "{:?}", gh.writes);
    assert!(git_in(&gh.repo, &["show", &format!("{prepared}:Web.csproj")]).contains("1.1.0"));
    assert!(
        git_in(
            &gh.repo,
            &["show", &format!("{prepared}:packages.lock.json")]
        )
        .contains("1.1.0")
    );
}
