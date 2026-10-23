//! Deterministic VirtualAssistant-compatible postprocessing; no LLM calls.
use anyhow::{Context, Result, ensure};
use regex::{NoExpand, Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use tokio_postgres::{Client, NoTls};

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct FilterConfig {
    pub remove_whole_text: Vec<String>,
    pub remove_suffix_regex: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Correction {
    pub id: i32,
    pub incorrect: String,
    pub correct: String,
    pub case_sensitive: bool,
    pub priority: i32,
}

pub struct Postprocessor {
    whole: Vec<String>,
    suffixes: Vec<Regex>,
    rules: Vec<(i32, Regex, String)>,
    slash: Regex,
}

impl Postprocessor {
    pub fn new(filters: FilterConfig, mut corrections: Vec<Correction>) -> Result<Self> {
        corrections.sort_by_key(|rule| (std::cmp::Reverse(rule.priority), rule.id));
        let suffixes = filters
            .remove_suffix_regex
            .iter()
            .map(|pattern| {
                RegexBuilder::new(&format!("(?:{pattern})$"))
                    .case_insensitive(true)
                    .build()
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut rules = Vec::new();
        for correction in corrections {
            ensure!(!correction.incorrect.is_empty(), "Empty correction pattern");
            let regex = RegexBuilder::new(&regex::escape(&correction.incorrect))
                .case_insensitive(!correction.case_sensitive)
                .build()?;
            rules.push((correction.id, regex, correction.correct));
        }
        Ok(Self {
            whole: filters
                .remove_whole_text
                .iter()
                .map(|text| text.trim().to_lowercase())
                .collect(),
            suffixes,
            rules,
            slash: Regex::new(r"(?i)\s*\blomeno\b\s*")?,
        })
    }

    pub fn apply(&self, text: &str) -> (String, Vec<i32>) {
        if text.trim().is_empty()
            || self
                .whole
                .iter()
                .any(|pattern| *pattern == text.trim().to_lowercase())
        {
            return (String::new(), Vec::new());
        }
        let mut result = text.to_owned();
        for _ in 0..10 {
            let before = result.clone();
            for regex in &self.suffixes {
                result = regex.replace_all(&result, "").into_owned();
            }
            if result == before {
                break;
            }
        }
        let mut used = Vec::new();
        for (id, regex, replacement) in &self.rules {
            if regex.is_match(&result) {
                result = regex
                    .replace_all(&result, NoExpand(replacement))
                    .into_owned();
                used.push(*id);
            }
        }
        result = self.slash.replace_all(&result, "/").into_owned();
        result = result.trim().to_owned();
        while result.contains("  ") {
            result = result.replace("  ", " ");
        }
        (result, used)
    }
}

pub struct Database {
    client: Client,
}

impl Database {
    pub async fn connect(url: &str) -> Result<Self> {
        let (client, connection) = tokio_postgres::connect(url, NoTls)
            .await
            .context("Cannot connect to dictation database")?;
        tokio::spawn(async move {
            if let Err(error) = connection.await {
                tracing::warn!(%error, "Dictation database connection closed");
            }
        });
        let name: String = client
            .query_one("SELECT current_database()", &[])
            .await?
            .get(0);
        ensure!(
            name != "virtual_assistant",
            "SIE requires an independent database; refusing the VirtualAssistant database"
        );
        Ok(Self { client })
    }

    pub async fn migrate(&self) -> Result<()> {
        self.client
            .batch_execute(include_str!("../migrations/001_dictation.sql"))
            .await?;
        Ok(())
    }

    pub async fn postprocessor(&self) -> Result<Postprocessor> {
        let row = self
            .client
            .query_one(
                "SELECT filters FROM postprocessing_settings WHERE id = TRUE",
                &[],
            )
            .await?;
        let filters: FilterConfig = serde_json::from_value(row.get(0))?;
        let rows = self.client.query("SELECT id, incorrect_text, correct_text, case_sensitive, priority FROM transcription_corrections WHERE is_active ORDER BY priority DESC, id", &[]).await?;
        Postprocessor::new(
            filters,
            rows.into_iter()
                .map(|row| Correction {
                    id: row.get(0),
                    incorrect: row.get(1),
                    correct: row.get(2),
                    case_sensitive: row.get(3),
                    priority: row.get(4),
                })
                .collect(),
        )
    }

    pub async fn save(
        &self,
        raw: &str,
        corrected: &str,
        model: &str,
        duration_ms: i64,
        delivered: bool,
        used: &[i32],
    ) -> Result<()> {
        self.client.execute("INSERT INTO dictation_history (raw_text, corrected_text, model_name, duration_ms, delivered) VALUES ($1,$2,$3,$4,$5)", &[&raw, &corrected, &model, &duration_ms, &delivered]).await?;
        for id in used {
            self.client.execute("INSERT INTO transcription_correction_usage (correction_id, usage_count) VALUES ($1,1) ON CONFLICT (correction_id) DO UPDATE SET usage_count = transcription_correction_usage.usage_count + 1, last_used_at = NOW()", &[id]).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: i32, incorrect: &str, correct: &str, sensitive: bool, priority: i32) -> Correction {
        Correction {
            id,
            incorrect: incorrect.into(),
            correct: correct.into(),
            case_sensitive: sensitive,
            priority,
        }
    }

    #[test]
    fn priority_case_sensitivity_and_literal_replacements_match_the_source_pipeline() {
        let processor = Postprocessor::new(
            FilterConfig::default(),
            vec![
                rule(2, "cloud", "oblak", false, 0),
                rule(1, "cloud kód", "Claude Code", false, 100),
                rule(3, "GitHub", "$repo", true, 0),
            ],
        )
        .unwrap();
        assert_eq!(
            processor.apply("CLOUD KÓD a GitHub a github"),
            ("Claude Code a $repo a github".into(), vec![1, 3])
        );
    }

    #[test]
    fn hallucinations_are_removed_without_dropping_legitimate_sentences() {
        let processor = Postprocessor::new(
            FilterConfig {
                remove_whole_text: vec!["Konec.".into()],
                remove_suffix_regex: vec![
                    r"\s*Titulky vytvořil[^.]*\.?\s*$".into(),
                    r"\s*(?-i:Konec\.)\s*$".into(),
                ],
            },
            vec![],
        )
        .unwrap();
        assert_eq!(processor.apply("  KONEC. ").0, "");
        assert_eq!(
            processor.apply("Konec konců je to správně.").0,
            "Konec konců je to správně."
        );
        assert_eq!(
            processor
                .apply("Dobrý den. Konec. Titulky vytvořil JohnyX.")
                .0,
            "Dobrý den."
        );
    }

    #[test]
    fn path_rewriting_runs_after_database_corrections_and_preserves_newlines() {
        let processor = Postprocessor::new(
            FilterConfig::default(),
            vec![rule(1, "llomeno", "lomeno", false, 1)],
        )
        .unwrap();
        assert_eq!(
            processor
                .apply("  Dokumenty  llomeno  přístupy\nNový řádek  ")
                .0,
            "Dokumenty/přístupy\nNový řádek"
        );
    }
}
