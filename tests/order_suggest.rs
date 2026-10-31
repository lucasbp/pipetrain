use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::{self, JoinHandle};

use pipetrain::command::order::graph::{Analysis, Chain, Edge, analyze};
use pipetrain::command::order::{names_of, suggest};
use pipetrain::config::ApiConfig;
use pipetrain::github::Client;
use pipetrain::publishers::Pairs;
use serde_json::{Value, json};

const FAKE: &str = "ghp_FAKE0123456789";

fn pairs(rows: &[(&str, &str, &str)]) -> Pairs {
    rows.iter()
        .map(|(publisher, consumer, package)| {
            (
                (publisher.to_string(), consumer.to_string()),
                package.to_string(),
            )
        })
        .collect()
}

fn slugs(chain: &Chain) -> Vec<&str> {
    chain.repos.iter().map(String::as_str).collect()
}

#[test]
fn a_component_is_ordered_publisher_first_with_ties_by_slug() {
    let found = analyze(&pairs(&[
        ("org/pub", "org/zed", "pkg.zed"),
        ("org/pub", "org/beta", "pkg.beta"),
        ("x/p", "x/q", "pkg.q"),
    ]));
    assert!(found.cycles.is_empty());
    assert_eq!(found.chains.len(), 2);
    assert_eq!(
        slugs(&found.chains[0]),
        vec!["org/pub", "org/beta", "org/zed"]
    );
    assert_eq!(slugs(&found.chains[1]), vec!["x/p", "x/q"]);
    assert_eq!(
        found.chains[0].edges,
        vec![
            Edge {
                consumer: "org/beta".to_string(),
                package: "pkg.beta".to_string(),
                publisher: "org/pub".to_string(),
            },
            Edge {
                consumer: "org/zed".to_string(),
                package: "pkg.zed".to_string(),
                publisher: "org/pub".to_string(),
            },
        ]
    );
}

#[test]
fn a_repo_without_edges_does_not_appear() {
    let found = analyze(&pairs(&[("org/a", "org/b", "pkg.b")]));
    let all: Vec<&str> = found.chains.iter().flat_map(slugs).collect();
    assert_eq!(all, vec!["org/a", "org/b"]);
}

#[test]
fn a_cycle_has_no_block_and_names_a_pair() {
    let found = analyze(&pairs(&[
        ("org/a", "org/b", "pkg.b"),
        ("org/b", "org/a", "pkg.a"),
        ("x/p", "x/q", "pkg.q"),
    ]));
    assert_eq!(
        found.cycles,
        vec![("org/a".to_string(), "org/b".to_string())]
    );
    assert_eq!(found.chains.len(), 1);
    assert_eq!(slugs(&found.chains[0]), vec!["x/p", "x/q"]);
}

#[test]
fn no_edges_means_no_chains_and_no_cycles() {
    assert_eq!(analyze(&Pairs::new()), Analysis::default());
}

struct Canned {
    status: &'static str,
    body: String,
}

fn canned(body: Value) -> Canned {
    Canned {
        status: "200 OK",
        body: body.to_string(),
    }
}

fn serve(replies: Vec<Canned>) -> (String, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let mut seen = Vec::new();
        for reply in replies {
            let (mut stream, _) = listener.accept().unwrap();
            seen.push(read_request(&mut stream));
            stream.write_all(render(&reply).as_bytes()).unwrap();
        }
        seen
    });
    (base, handle)
}

fn read_request(stream: &mut TcpStream) -> String {
    let mut data = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = stream.read(&mut chunk).unwrap();
        data.extend_from_slice(&chunk[..read]);
        let text = String::from_utf8_lossy(&data).to_string();
        if read == 0 || body_complete(&text) {
            return text;
        }
    }
}

fn body_complete(text: &str) -> bool {
    let Some((head, body)) = text.split_once("\r\n\r\n") else {
        return false;
    };
    let wanted = head
        .lines()
        .find_map(|line| {
            line.to_lowercase()
                .strip_prefix("content-length:")
                .map(str::to_string)
        })
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    body.len() >= wanted
}

fn render(reply: &Canned) -> String {
    format!(
        "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        reply.status,
        reply.body.len(),
        reply.body
    )
}

fn repo_replies(csproj: &str, text: &str) -> Vec<Canned> {
    vec![
        canned(
            json!({"data": {"repository": {"defaultBranchRef": {"target": {"oid": "abc123"}}}}}),
        ),
        canned(json!({"tree": [
            {"type": "blob", "path": "README.md"},
            {"type": "blob", "path": csproj}
        ], "truncated": false})),
        canned(json!({"data": {"repository": {"f0": {"text": text}}}})),
    ]
}

#[test]
fn two_repos_over_the_fake_server_print_the_block_and_leak_no_token() {
    let mut replies = repo_replies(
        "src/Acme.Sdk.csproj",
        r#"<PackageReference Include="Acme.Util" Version="1.0.0" />"#,
    );
    replies.extend(repo_replies(
        "src/Acme.Util.csproj",
        "<GeneratePackageOnBuild>true</GeneratePackageOnBuild>",
    ));
    let (base, server) = serve(replies);
    let cfg = ApiConfig {
        max_requests: 50,
        max_per_minute: 1000,
        min_remaining: 0,
    };
    let mut client = Client::with_base_url(FAKE.to_string(), &cfg, false, &base);
    let names = names_of(["Acme/Sdk".to_string(), "Acme/Util".to_string()]);

    let found = suggest(&mut client, &names).unwrap();
    let seen = server.join().unwrap();

    assert_eq!(
        found.stdout,
        "# Acme/Sdk references acme.util, published by Acme/Util\n[[order]]\nrepos = [\"Acme/Util\", \"Acme/Sdk\"]\n"
    );
    assert!(found.stderr.is_empty());
    assert_eq!(found.exit, 0);
    assert_eq!(seen.len(), 6);
    assert!(
        seen.iter()
            .all(|r| r.to_lowercase().contains("authorization"))
    );
    let printed = format!("{}{}", found.stdout, found.stderr.join("\n")).to_lowercase();
    assert!(!printed.contains("authorization"));
    assert!(!printed.contains(&FAKE.to_lowercase()));
}
