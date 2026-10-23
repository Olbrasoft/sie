//! Only runs against an explicitly configured, dedicated test database.
use sie::corrections::Database;

#[tokio::test]
#[ignore = "requires SIE_TEST_DB pointing to a database named test_sie_*"]
async fn database_rules_filters_usage_and_history_round_trip() {
    let url = std::env::var("SIE_TEST_DB").expect("SIE_TEST_DB is required");
    let (client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(async move {
        connection.await.unwrap();
    });
    let name: String = client
        .query_one("SELECT current_database()", &[])
        .await
        .unwrap()
        .get(0);
    assert!(
        name.starts_with("test_sie_"),
        "Refusing to modify a non-test database"
    );
    let database = Database::connect(&url).await.unwrap();
    database.migrate().await.unwrap();
    database.migrate().await.unwrap();
    client.batch_execute("INSERT INTO transcription_corrections (id,incorrect_text,correct_text,case_sensitive,priority,is_active) VALUES (1,'cloud kód','Claude Code',false,100,true),(2,'cloud','oblak',false,0,true),(3,'vyspru','Whisper',false,0,false) ON CONFLICT (id) DO UPDATE SET is_active=EXCLUDED.is_active; UPDATE postprocessing_settings SET filters='{}' WHERE id=TRUE;").await.unwrap();
    let processor = database.postprocessor().await.unwrap();
    let (text, used) = processor.apply("CLOUD KÓD lomeno vyspru");
    assert_eq!(text, "Claude Code/vyspru");
    assert_eq!(used, vec![1]);
    let filters = serde_json::json!({"removeWholeText":["Konec."]});
    client
        .execute(
            "UPDATE postprocessing_settings SET filters=$1 WHERE id=TRUE",
            &[&filters],
        )
        .await
        .unwrap();
    assert_eq!(
        database.postprocessor().await.unwrap().apply("KONEC.").0,
        ""
    );
    database
        .save(
            "CLOUD KÓD lomeno vyspru",
            &text,
            "test-model",
            1234,
            true,
            &used,
        )
        .await
        .unwrap();
    let row = client
        .query_one(
            "SELECT corrected_text,delivered FROM dictation_history ORDER BY id DESC LIMIT 1",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, String>(0), text);
    assert!(row.get::<_, bool>(1));
    let usage: i64 = client
        .query_one(
            "SELECT usage_count FROM transcription_correction_usage WHERE correction_id=1",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert!(usage >= 1);
}
