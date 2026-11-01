use super::*;

#[tokio::test]
async fn failed_key_press_still_releases_both_keys_and_stops_the_session() {
    let bus = Bus::new();
    let state = Arc::new(Mutex::new(State {
        count: 5,
        fail_press: true,
        ..Default::default()
    }));
    let _server = services(&bus, state.clone(), vec![8, 23]).await;
    let output = switch(&bus, &schemas(), 0).await;
    assert!(!output.status.success());
    let state = state.lock().unwrap();
    assert_eq!(
        state.keys,
        [(125, true), (79, true), (79, false), (125, false)]
    );
    assert_eq!(state.stops, 1);
}

#[tokio::test]
async fn unavailable_workspaces_read_the_live_count_and_emit_no_input() {
    let bus = Bus::new();
    let state = Arc::new(Mutex::new(State {
        count: 4,
        current: 3,
        ..Default::default()
    }));
    let _server = services(&bus, state.clone(), vec![8, 23]).await;
    let desktop = Desktop::connect(&bus.connection().await).await.unwrap();
    assert!(!desktop.switch(4).await.unwrap());
    state.lock().unwrap().count = 3;
    assert!(!desktop.switch(3).await.unwrap());
    assert!(desktop.switch(5).await.is_err());
    desktop.stop().await.unwrap();
    let state = state.lock().unwrap();
    assert!(state.keys.is_empty());
    assert_eq!(state.current, 3);
    assert_eq!((state.starts, state.stops), (1, 1));
}

#[tokio::test]
async fn binary_switches_all_five_targets_and_releases_every_key() {
    let bus = Bus::new();
    let state = Arc::new(Mutex::new(State {
        count: 5,
        ..Default::default()
    }));
    let _server = services(&bus, state.clone(), vec![8, 23]).await;
    let schemas = schemas();
    for index in 0..5 {
        let output = switch(&bus, &schemas, index).await;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(state.lock().unwrap().current, index as i32);
    }
    let state = state.lock().unwrap();
    assert_eq!((state.starts, state.stops), (5, 5));
    for (index, chord) in state.keys.as_chunks::<4>().0.iter().enumerate() {
        assert_eq!(
            *chord,
            [
                (125, true),
                ([79, 80, 81, 75, 76][index], true),
                ([79, 80, 81, 75, 76][index], false),
                (125, false)
            ]
        );
    }
}

#[tokio::test]
async fn duplicate_instance_cannot_replace_the_running_application() {
    let bus = Bus::new();
    let first = bus.connection().await;
    let second = bus.connection().await;
    sie::claim_instance(&first).await.unwrap();
    assert!(sie::claim_instance(&second).await.is_err());
    // The first connection retains ownership after the rejected attempt.
    let proxy = zbus::fdo::DBusProxy::new(&first).await.unwrap();
    assert_eq!(
        proxy
            .get_name_owner(sie::BUS_NAME.try_into().unwrap())
            .await
            .unwrap()
            .as_str(),
        first.unique_name().unwrap().as_str()
    );
}
