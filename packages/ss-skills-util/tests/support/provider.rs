// Standalone native process fixture, compiled once by the repository test preparation phase.
// It requires no installed provider CLI or credentials and never reaches a remote service.
use std::env;
use std::fs;
use std::io::{self, Read, Write};

fn main() {
    let args: Vec<_> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("pipe") => {
            // Exceed ordinary pipe capacity before reading, catching sequential pipe deadlocks.
            io::stdout().write_all(&vec![b'o'; 256 * 1024]).unwrap();
            io::stderr().write_all(&vec![b'e'; 256 * 1024]).unwrap();
            let mut bytes = Vec::new();
            io::stdin().read_to_end(&mut bytes).unwrap();
            io::stdout().write_all(&bytes).unwrap();
        }
        Some("arguments") => {
            for arg in args.iter().skip(1) {
                writeln!(io::stdout(), "{arg}").unwrap();
            }
        }
        Some("account") => {
            assert_eq!(args, ["account", "get-access-token", "--resource", "499b84ac-1321-427f-aa17-267ca6975798",
                "--query", "accessToken", "--output", "tsv", "--tenant", "tenant with spaces & punctuation"]);
            println!("fixture-oauth");
        }
        Some("api") => github(&args),
        _ => panic!("Unexpected fixture arguments: {args:?}"),
    }
}

fn github(args: &[String]) {
    let path = &args[1];
    let method = &args[args.iter().position(|arg| arg == "--method").unwrap() + 1];
    let root = env::var_os("FIXTURE_ROOT").unwrap();
    let claim = std::path::Path::new(&root).join("remote-claim.json");
    let date = env::var("FIXTURE_COMMIT_DATE").unwrap();
    let response = match (method.as_str(), path.as_str()) {
        ("GET", "user") => r#"{"id":42,"login":"reviewer","type":"User"}"#.to_owned(),
        ("GET", "repos/example/widgets") => r#"{"id":123}"#.to_owned(),
        ("GET", "repos/example/widgets/pulls?state=open&per_page=100") => {
            if env::var_os("FIXTURE_NO_CANDIDATES").is_some() { "[[]]".to_owned() }
            else { "[[],[{\"number\":10,\"state\":\"open\",\"draft\":false,\"title\":\"fixture\"}]]".to_owned() }
        }
        ("GET", "repos/example/widgets/pulls/10") => r#"{"state":"open","draft":false,"user":{"id":99,"login":"author"}}"#.to_owned(),
        ("GET", "repos/example/widgets/pulls/10/requested_reviewers?per_page=100") => r#"[{"users":[],"teams":[]}]"#.to_owned(),
        ("GET", "repos/example/widgets/pulls/10/reviews?per_page=100") => "[[]]".to_owned(),
        ("POST", "graphql") => {
            let mut input = String::new();
            io::stdin().read_to_string(&mut input).unwrap();
            assert!(input.contains("committedDate"));
            format!(r#"{{"data":{{"repository":{{"pullRequest":{{"commits":{{"nodes":[{{"commit":{{"committedDate":"{date}"}}}}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}}}"#)
        }
        ("POST", "repos/example/widgets/issues/10/comments") => {
            let mut input = String::new();
            io::stdin().read_to_string(&mut input).unwrap();
            fs::write(&claim, input).unwrap();
            if env::var_os("FIXTURE_LOST_RESPONSE").is_some() {
                eprintln!("transport closed after request");
                std::process::exit(1);
            }
            r#"{"id":55}"#.to_owned()
        }
        ("GET", "repos/example/widgets/issues/10/comments?per_page=100") => {
            match fs::read_to_string(&claim) {
                Ok(input) => {
                    let body = input.strip_prefix("{\"body\":").unwrap().strip_suffix('}').unwrap();
                    format!(r#"[[{{"id":55,"body":{body},"created_at":"{date}","user":{{"id":42,"login":"reviewer"}}}}]]"#)
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => "[[]]".to_owned(),
                Err(error) => panic!("{error}"),
            }
        }
        ("POST", "repos/example/widgets/pulls/10/requested_reviewers") => {
            let mut input = String::new();
            io::stdin().read_to_string(&mut input).unwrap();
            assert_eq!(input, r#"{"reviewers":["reviewer"]}"#);
            fs::write(std::path::Path::new(&root).join("registration.json"), input).unwrap();
            "{}".to_owned()
        }
        _ => panic!("Unexpected fixture operation: {method} {path}"),
    };
    print!("{response}");
}
