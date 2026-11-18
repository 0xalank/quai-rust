mod choice_window_support;
#[test]
fn official_token_choice_window_warmup_rollover_forks_and_atomic_recovery() {
    let v = serde_json::from_str(include_str!(
        "../../../fixtures/protocol/choice-window.json"
    ))
    .unwrap();
    let dir = std::env::temp_dir().join(format!("quai-choice-window-test-{}", std::process::id()));
    assert!(!dir.exists());
    std::fs::create_dir(&dir).unwrap();
    let report = choice_window_support::verify(&v, &dir);
    assert_eq!(report["updates"], 4216);
    println!("{report}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn token_choice_recovery_rejects_malformed_rows_and_does_not_select_them() {
    use quai_consensus::{
        choice_window::{Choice, Window},
        rules::Uint,
    };
    use quai_store::state_db::StateDatabase;
    let dir =
        std::env::temp_dir().join(format!("quai-choice-window-corrupt-{}", std::process::id()));
    assert!(!dir.exists());
    std::fs::create_dir(&dir).unwrap();
    let db = StateDatabase::open(&dir.join("state.redb"), [13; 32]).unwrap();
    let initial = Window::empty_at([11; 32], 261999);
    let mut saved = None;
    let seed = db
        .commit(None, None, [11; 32], b"conditional seed", true, |ws| {
            saved = Some(initial.persist(ws, None)?);
            Ok(())
        })
        .unwrap();
    let seed_window = saved.unwrap();
    let next = seed_window
        .advance(
            [12; 32],
            seed_window.block(),
            262000,
            Choice {
                quai: 1,
                qi: 0,
                difficulty: Uint::ONE,
            },
        )
        .unwrap();
    let head = Some(seed.version.id);
    let child = db
        .commit(head, head, [12; 32], b"good window", false, |ws| {
            next.persist(ws, Some(&seed_window))?;
            Ok(())
        })
        .unwrap();
    for (index, row) in [vec![1u8; 47], vec![0u8; 48], vec![1u8; 48]]
        .iter()
        .enumerate()
    {
        let bad = db
            .commit(
                head,
                Some(child.version.id),
                [20 + index as u8; 32],
                b"bad row",
                false,
                |ws| ws.put(b"prime-token-choices-v1", &[1, 0, 0], row),
            )
            .unwrap();
        assert!(Window::load(&db.snapshot(Some(bad.version.id)).unwrap()).is_err());
        assert_eq!(db.head().unwrap(), head);
    }
    assert_eq!(
        Window::load(&db.snapshot(head).unwrap())
            .unwrap()
            .logical_digest(),
        seed_window.logical_digest()
    );
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}
