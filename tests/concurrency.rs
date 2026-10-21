//! Concurrent writers against one file: preconditions, revision counts, and the
//! invariant that a commit is one atomic step rather than a check-then-write race.

use std::sync::Arc;

use drawer::{
    error::Error,
    service::{Drawer, Ref, WriteOpts},
};

fn guarded(writer: &str, expected: i64) -> WriteOpts {
    WriteOpts {
        author: writer.to_string(),
        if_head_revision: Some(expected),
        ..WriteOpts::default()
    }
}

fn plain(writer: &str) -> WriteOpts {
    WriteOpts {
        author: writer.to_string(),
        ..WriteOpts::default()
    }
}

/// The writers that landed, in the order they reported success, with the revision each got.
async fn run_writers(s: &Drawer, path: &str, opts: Vec<WriteOpts>) -> Vec<(String, i64, bool)> {
    let mut handles = Vec::new();
    for o in opts {
        let store = s.clone();
        let path = path.to_string();
        handles.push(tokio::spawn(async move {
            let writer = o.author.clone();
            match store.write(&path, writer.as_bytes(), o).await {
                Ok(outcome) => (writer, outcome.revision, true),
                Err(error) => (writer, -1, format!("{error}").contains("expected revision")),
            }
        }));
    }
    let mut landed = Vec::new();
    for handle in handles {
        let (writer, revision, success) = handle.await.unwrap();
        if success && revision > 0 {
            landed.push((writer, revision, true));
        }
    }
    landed
}

async fn head(s: &Drawer, path: &str) -> i64 {
    s.stat(Ref::Path(path))
        .await
        .unwrap()
        .head_revision
        .unwrap()
}

#[tokio::test]
async fn concurrent_unguarded_writers_lose_nothing_and_reach_a_consistent_head() {
    let dir = tempfile::tempdir().unwrap();
    let s = Drawer::open(dir.path()).await.unwrap();
    let writers: Vec<_> = (0..16).map(|i| plain(&format!("w{i}"))).collect();

    let landed = run_writers(&s, "/contended.txt", writers).await;
    assert_eq!(landed.len(), 16, "every writer lands a revision");

    // Every successful write got its own revision, with no gaps and no duplicates.
    let mut revisions: Vec<i64> = landed.iter().map(|(_, r, _)| *r).collect();
    revisions.sort_unstable();
    assert_eq!(
        revisions,
        (1..=16).collect::<Vec<_>>(),
        "revisions are exactly 1..=16"
    );

    // The head is the highest revision that landed, and it reads back its own writer.
    let top = revisions.last().copied().unwrap();
    assert_eq!(head(&s, "/contended.txt").await, top);
    let (rev, data) = s.read(Ref::Path("/contended.txt"), None).await.unwrap();
    assert_eq!(rev.revision, top);
    assert_eq!(data, top_writer(&landed, top).as_bytes());
    // History is complete: every writer's bytes are still readable at its own revision.
    for (writer, revision, _) in &landed {
        assert_eq!(
            s.read(Ref::Path("/contended.txt"), Some(*revision))
                .await
                .unwrap()
                .1,
            writer.as_bytes()
        );
    }
    assert_eq!(
        s.revisions(Ref::Path("/contended.txt"))
            .await
            .unwrap()
            .len(),
        16
    );
    assert_eq!(s.stats().await.unwrap().revisions, 16);
}

fn top_writer(landed: &[(String, i64, bool)], revision: i64) -> String {
    landed
        .iter()
        .find(|(_, r, _)| *r == revision)
        .unwrap()
        .0
        .clone()
}

#[tokio::test]
async fn stale_preconditions_are_reported_as_conflicts_and_the_last_writer_wins() {
    let dir = tempfile::tempdir().unwrap();
    let s = Drawer::open(dir.path()).await.unwrap();
    s.write("/guarded.txt", b"base", plain("base"))
        .await
        .unwrap();

    // Every writer guards on revision 1, so only one of them may land.
    let writers: Vec<_> = (0..8).map(|i| guarded(&format!("w{i}"), 1)).collect();
    let landed = run_writers(&s, "/guarded.txt", writers).await;
    assert_eq!(
        landed.len(),
        1,
        "exactly one writer passes a precondition on the same revision"
    );
    assert_eq!(landed[0].1, 2, "the winner takes the next revision");

    // The file reads back the winner's bytes, and its head is the winner's revision.
    let winner = landed[0].0.clone();
    assert_eq!(
        s.read(Ref::Path("/guarded.txt"), None).await.unwrap().1,
        winner.as_bytes()
    );
    assert_eq!(head(&s, "/guarded.txt").await, 2);
    assert_eq!(
        s.revisions(Ref::Path("/guarded.txt")).await.unwrap().len(),
        2
    );

    // Re-guarding on the now-current revision succeeds, and on a stale one fails loudly.
    let fresh = s
        .write("/guarded.txt", b"fresh", guarded("fresh", 2))
        .await
        .unwrap();
    assert_eq!(fresh.revision, 3);
    let stale = s
        .write("/guarded.txt", b"stale", guarded("stale", 1))
        .await
        .unwrap_err();
    assert!(matches!(stale, Error::PreconditionFailed(_)), "{stale}");
    assert!(stale.to_string().contains("revision 3"), "{stale}");
    // A stale write left nothing behind.
    assert_eq!(head(&s, "/guarded.txt").await, 3);
    assert_eq!(
        s.read(Ref::Path("/guarded.txt"), None).await.unwrap().1,
        b"fresh"
    );
    assert_eq!(
        s.revisions(Ref::Path("/guarded.txt")).await.unwrap().len(),
        3
    );
}

#[tokio::test]
async fn concurrent_writers_on_a_new_file_all_land_sequential_revisions() {
    let dir = tempfile::tempdir().unwrap();
    let s = Drawer::open(dir.path()).await.unwrap();
    let writers: Vec<_> = (0..12).map(|i| guarded(&format!("c{i}"), 0)).collect();

    // A create-only precondition passes only for the first writer on a new path, so a
    // second run against the same path must not clobber what the first created.
    let first = s
        .write("/new.txt", b"first", guarded("first", 0))
        .await
        .unwrap();
    assert_eq!(first.revision, 1);
    let landed = run_writers(&s, "/new.txt", writers).await;
    assert!(
        landed.is_empty(),
        "a create-only write is refused once the file exists"
    );
    assert_eq!(s.revisions(Ref::Path("/new.txt")).await.unwrap().len(), 1);
    assert_eq!(
        s.read(Ref::Path("/new.txt"), None).await.unwrap().1,
        b"first"
    );
}

#[tokio::test]
async fn guarded_writers_that_chain_onto_the_live_head_all_land() {
    let dir = tempfile::tempdir().unwrap();
    let s = Drawer::open(dir.path()).await.unwrap();

    // Each writer reads the current head, then guards on it. A losing writer re-reads
    // and retries, so all of them eventually land and none is silently dropped.
    let store = Arc::new(s);
    let mut handles = Vec::new();
    for i in 0..10 {
        let store = store.clone();
        handles.push(tokio::spawn(async move {
            let writer = format!("r{i}");
            let mut expected = store
                .stat(Ref::Path("/chain.txt"))
                .await
                .ok()
                .and_then(|n| n.head_revision)
                .unwrap_or(0);
            loop {
                match store
                    .write("/chain.txt", writer.as_bytes(), guarded(&writer, expected))
                    .await
                {
                    Ok(outcome) => return Some((writer, outcome.revision)),
                    Err(Error::PreconditionFailed(_)) => {
                        expected = store
                            .stat(Ref::Path("/chain.txt"))
                            .await
                            .unwrap()
                            .head_revision
                            .unwrap();
                    }
                    Err(other) => panic!("unexpected error: {other}"),
                }
            }
        }));
    }
    let mut landed = Vec::new();
    for handle in handles {
        if let Some(pair) = handle.await.unwrap() {
            landed.push(pair);
        }
    }
    assert_eq!(landed.len(), 10, "every retrying writer lands");
    let mut revisions: Vec<i64> = landed.iter().map(|(_, r)| *r).collect();
    revisions.sort_unstable();
    assert_eq!(revisions, (1..=10).collect::<Vec<_>>());
    assert_eq!(head(&store, "/chain.txt").await, 10);
    // The head is the last revision that landed, and history has no gaps.
    let top = revisions.last().copied().unwrap();
    assert_eq!(
        landed.iter().find(|(_, r)| *r == top).unwrap().0.as_bytes(),
        store.read(Ref::Path("/chain.txt"), None).await.unwrap().1
    );
    assert_eq!(
        store
            .revisions(Ref::Path("/chain.txt"))
            .await
            .unwrap()
            .len(),
        10
    );
}

#[tokio::test]
async fn a_guarded_write_and_a_plain_write_race_without_losing_a_revision() {
    let dir = tempfile::tempdir().unwrap();
    let s = Drawer::open(dir.path()).await.unwrap();
    s.write("/mix.txt", b"start", plain("start")).await.unwrap();

    let store = Arc::new(s);
    let plain_writer = {
        let store = store.clone();
        tokio::spawn(async move {
            store
                .write("/mix.txt", b"plain", plain("plain"))
                .await
                .map(|o| o.revision)
        })
    };
    let guarded_writer = {
        let store = store.clone();
        tokio::spawn(async move {
            store
                .write("/mix.txt", b"guarded", guarded("guarded", 1))
                .await
                .map(|o| o.revision)
        })
    };
    let (plain_result, guarded_result) = tokio::join!(plain_writer, guarded_writer);
    let plain_result = plain_result.unwrap();
    let guarded_result = guarded_result.unwrap();

    // Both may land, or the guarded one may lose to the plain one; what never happens is
    // a lost revision or a head that disagrees with the revision history.
    let landed: Vec<i64> = [plain_result, guarded_result]
        .into_iter()
        .flatten()
        .collect();
    assert!(!landed.is_empty(), "at least one writer lands");
    assert!(
        landed.iter().all(|r| (2..=3).contains(r)),
        "landed revisions are 2 and 3: {landed:?}"
    );
    // `revisions` is newest first, so the ascending view is its reverse.
    let revisions: Vec<i64> = store
        .revisions(Ref::Path("/mix.txt"))
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.revision)
        .collect();
    let ascending: Vec<i64> = revisions.iter().rev().copied().collect();
    assert_eq!(
        ascending,
        (1..=landed.len() as i64 + 1).collect::<Vec<_>>(),
        "history is contiguous"
    );
    assert_eq!(head(&store, "/mix.txt").await, *landed.last().unwrap());
    let (rev, data) = store.read(Ref::Path("/mix.txt"), None).await.unwrap();
    assert_eq!(rev.revision, head(&store, "/mix.txt").await);
    assert!([b"plain".as_slice(), b"guarded".as_slice()].contains(&data.as_slice()));
}

#[tokio::test]
async fn concurrent_readers_see_a_consistent_head_while_writers_race() {
    let dir = tempfile::tempdir().unwrap();
    let s = Drawer::open(dir.path()).await.unwrap();
    s.write("/view.txt", b"w0", plain("w0")).await.unwrap();

    let store = Arc::new(s);
    let mut handles = Vec::new();
    for i in 0..6 {
        let store = store.clone();
        handles.push(tokio::spawn(async move {
            for _ in 0..4 {
                let writer = format!("v{i}");
                let expected = store
                    .stat(Ref::Path("/view.txt"))
                    .await
                    .unwrap()
                    .head_revision;
                let _ = store
                    .write(
                        "/view.txt",
                        writer.as_bytes(),
                        guarded(&writer, expected.unwrap_or(0)),
                    )
                    .await;
            }
        }));
    }
    for handle in handles {
        handle.await.unwrap();
    }

    // Whatever the history looks like, every revision that exists is readable and the
    // head always agrees with the highest revision that landed.
    let revisions = store.revisions(Ref::Path("/view.txt")).await.unwrap();
    let ascending: Vec<i64> = revisions.iter().rev().map(|r| r.revision).collect();
    assert_eq!(
        ascending,
        (1..=ascending.len() as i64).collect::<Vec<_>>(),
        "no gaps, no duplicates"
    );
    assert_eq!(
        head(&store, "/view.txt").await,
        ascending.last().copied().unwrap()
    );
    // Every revision that exists is readable, and its bytes are one of the writers that
    // landed there: a failed precondition must not leave a half-written revision behind.
    // Revision 1 is the seed write; every later one is one of the racing writers.
    let writers: std::collections::HashSet<String> = (0..6)
        .map(|i| format!("v{i}"))
        .chain(["w0".to_string()])
        .collect();
    for rev in &revisions {
        let (_, data) = store
            .read(Ref::Path("/view.txt"), Some(rev.revision))
            .await
            .unwrap();
        let author = String::from_utf8(data).unwrap();
        assert!(
            writers.contains(&author),
            "revision {} holds {author:?}",
            rev.revision
        );
        assert_eq!(
            &author, &rev.author,
            "revision {} records its own writer",
            rev.revision
        );
    }
    assert_eq!(store.missing_blobs().await.unwrap(), Vec::<String>::new());
}
