//! Starts, shows or ends the key-leak stop of a running operator, through its database file.
//! The operator reads the flag on every lease and clearance request, so no restart is needed.
//!
//! ```text
//! cargo run -p nymira-operator --example emergency -- <operator.db> set "<notice shown to clients>"
//! cargo run -p nymira-operator --example emergency -- <operator.db> show
//! cargo run -p nymira-operator --example emergency -- <operator.db> clear
//! ```

use std::path::Path;

use nymira_operator::store::{self, Store};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = || -> ! {
        eprintln!("usage: emergency <operator.db> set <notice> | show | clear");
        std::process::exit(2)
    };
    let (Some(db), Some(command)) = (args.first(), args.get(1)) else {
        usage()
    };
    let store = Store::open(Path::new(db)).expect("open the database");
    match (command.as_str(), args.get(2)) {
        ("set", Some(notice)) => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after 1970")
                .as_secs();
            let notice = notice.clone();
            store
                .run(move |c| store::set_emergency(c, &notice, now))
                .await
                .expect("write the flag");
            eprintln!("emergency set: new leases and clearances stop now");
        }
        ("show", None) => {
            let notice = store.run(|c| store::emergency(c)).await.expect("read");
            eprintln!("{}", notice.as_deref().unwrap_or("no emergency"));
        }
        ("clear", None) => {
            store
                .run(|c| store::clear_emergency(c))
                .await
                .expect("clear the flag");
            eprintln!("emergency cleared");
        }
        _ => usage(),
    }
}
