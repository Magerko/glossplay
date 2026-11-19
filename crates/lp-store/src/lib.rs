use lp_core::{Cue, Ms};
use rusqlite::{Connection, OptionalExtension, params};
use std::fmt;
use std::path::Path;

#[derive(Debug)]
pub enum StoreError {
    Sqlite(rusqlite::Error),
    Corrupt(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Sqlite(e) => write!(f, "database error: {e}"),
            StoreError::Corrupt(what) => write!(f, "database is corrupt: {what}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError::Sqlite(e)
    }
}

type Result<T> = std::result::Result<T, StoreError>;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NewWord {
    pub word: String,
    pub lang: String,
    pub translation: String,
    pub note: String,
    pub sentence: String,
    pub sentence_translation: String,
    pub media_title: String,
    pub media_path: String,
    pub pos_ms: Ms,
    pub cue_start_ms: Ms,
    pub cue_end_ms: Ms,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VocabEntry {
    pub id: i64,
    pub word: NewWord,
    pub created_at: i64,
    pub anki_note_id: Option<i64>,
}

/// Cached speech recognition: finished windows (empty for a pre-v2 row, where the
/// windows before `covered` are the finished ones) and every cue found so far.
#[derive(Debug, Clone, PartialEq)]
pub struct Transcript {
    pub covered: Ms,
    pub done: Vec<bool>,
    pub cues: Vec<Cue>,
}

pub struct Store {
    conn: Connection,
}

const MIGRATIONS: &[&str] = &[
    "
CREATE TABLE translation (
  provider TEXT NOT NULL,
  src_lang TEXT NOT NULL,
  dst_lang TEXT NOT NULL,
  text     TEXT NOT NULL,
  result   TEXT NOT NULL,
  PRIMARY KEY (provider, src_lang, dst_lang, text)
) WITHOUT ROWID;

CREATE TABLE transcript (
  media_key  TEXT NOT NULL,
  engine     TEXT NOT NULL,
  lang       TEXT NOT NULL,
  covered_ms INTEGER NOT NULL,
  cues       TEXT NOT NULL,
  PRIMARY KEY (media_key, engine, lang)
) WITHOUT ROWID;

CREATE TABLE vocab (
  id INTEGER PRIMARY KEY,
  word TEXT NOT NULL,
  word_key TEXT NOT NULL,
  lang TEXT NOT NULL,
  translation TEXT NOT NULL DEFAULT '',
  note TEXT NOT NULL DEFAULT '',
  sentence TEXT NOT NULL,
  sentence_translation TEXT NOT NULL DEFAULT '',
  media_title TEXT NOT NULL,
  media_path TEXT NOT NULL,
  pos_ms INTEGER NOT NULL,
  cue_start_ms INTEGER NOT NULL,
  cue_end_ms INTEGER NOT NULL,
  created_at INTEGER NOT NULL,
  anki_note_id INTEGER,
  UNIQUE (word_key, lang)
);
",
    // One '0'/'1' per recognition window; empty means only the prefix up to covered_ms is done.
    "ALTER TABLE transcript ADD COLUMN done TEXT NOT NULL DEFAULT '';",
];

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Store> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Store> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        let version = usize::try_from(version).unwrap_or(usize::MAX);
        if version > MIGRATIONS.len() {
            return Err(StoreError::Corrupt(format!(
                "schema version {version} is newer than this build supports"
            )));
        }
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(version) {
            conn.execute_batch(&format!(
                "BEGIN; {sql} PRAGMA user_version = {}; COMMIT;",
                i + 1
            ))?;
        }
        Ok(Store { conn })
    }

    pub fn get_translations(
        &self,
        provider: &str,
        src: &str,
        dst: &str,
        texts: &[String],
    ) -> Result<Vec<Option<String>>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT result FROM translation WHERE provider=?1 AND src_lang=?2 AND dst_lang=?3 AND text=?4",
        )?;
        texts
            .iter()
            .map(|t| {
                Ok(stmt
                    .query_row(params![provider, src, dst, t], |r| r.get(0))
                    .optional()?)
            })
            .collect()
    }

    pub fn put_translations(
        &mut self,
        provider: &str,
        src: &str,
        dst: &str,
        pairs: &[(String, String)],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR REPLACE INTO translation (provider, src_lang, dst_lang, text, result)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for (text, result) in pairs {
                stmt.execute(params![provider, src, dst, text, result])?;
            }
        }
        Ok(tx.commit()?)
    }

    pub fn load_transcript(
        &self,
        media_key: &str,
        engine: &str,
        lang: &str,
    ) -> Result<Option<Transcript>> {
        let row: Option<(Ms, String, String)> = self
            .conn
            .query_row(
                "SELECT covered_ms, cues, done FROM transcript WHERE media_key=?1 AND engine=?2 AND lang=?3",
                params![media_key, engine, lang],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((covered, json, done)) = row else {
            return Ok(None);
        };
        let raw: Vec<(Ms, Ms, String)> = serde_json::from_str(&json)
            .map_err(|e| StoreError::Corrupt(format!("transcript for {media_key}: {e}")))?;
        let cues = raw
            .into_iter()
            .map(|(start, end, text)| Cue { start, end, text })
            .collect();
        Ok(Some(Transcript {
            covered,
            done: done.chars().map(|c| c == '1').collect(),
            cues,
        }))
    }

    /// `done` has one flag per recognition window; pass an empty slice to store only the prefix.
    pub fn save_transcript(
        &self,
        media_key: &str,
        engine: &str,
        lang: &str,
        covered: Ms,
        done: &[bool],
        cues: &[Cue],
    ) -> Result<()> {
        let raw: Vec<(Ms, Ms, &str)> = cues
            .iter()
            .map(|c| (c.start, c.end, c.text.as_str()))
            .collect();
        let json = serde_json::to_string(&raw).expect("tuples of numbers and strings serialize");
        let mask: String = done.iter().map(|d| if *d { '1' } else { '0' }).collect();
        self.conn.execute(
            "INSERT OR REPLACE INTO transcript (media_key, engine, lang, covered_ms, cues, done)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![media_key, engine, lang, covered, json, mask],
        )?;
        Ok(())
    }

    /// Saving a word again updates the entry and returns its existing id.
    pub fn save_word(&self, w: &NewWord) -> Result<i64> {
        let id = self.conn.query_row(
            "INSERT INTO vocab (word, word_key, lang, translation, note, sentence, sentence_translation,
                                media_title, media_path, pos_ms, cue_start_ms, cue_end_ms, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, unixepoch())
             ON CONFLICT (word_key, lang) DO UPDATE SET
               translation = CASE WHEN excluded.translation != '' THEN excluded.translation ELSE translation END,
               note = CASE WHEN excluded.note != '' THEN excluded.note ELSE note END,
               sentence = excluded.sentence,
               sentence_translation = excluded.sentence_translation,
               media_title = excluded.media_title,
               media_path = excluded.media_path,
               pos_ms = excluded.pos_ms,
               cue_start_ms = excluded.cue_start_ms,
               cue_end_ms = excluded.cue_end_ms
             RETURNING id",
            params![
                w.word,
                w.word.to_lowercase(),
                w.lang,
                w.translation,
                w.note,
                w.sentence,
                w.sentence_translation,
                w.media_title,
                w.media_path,
                w.pos_ms,
                w.cue_start_ms,
                w.cue_end_ms
            ],
            |r| r.get(0),
        )?;
        Ok(id)
    }

    /// Newest first.
    pub fn list_words(&self) -> Result<Vec<VocabEntry>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, word, lang, translation, note, sentence, sentence_translation, media_title,
                    media_path, pos_ms, cue_start_ms, cue_end_ms, created_at, anki_note_id
             FROM vocab ORDER BY created_at DESC, id DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(VocabEntry {
                id: r.get(0)?,
                word: NewWord {
                    word: r.get(1)?,
                    lang: r.get(2)?,
                    translation: r.get(3)?,
                    note: r.get(4)?,
                    sentence: r.get(5)?,
                    sentence_translation: r.get(6)?,
                    media_title: r.get(7)?,
                    media_path: r.get(8)?,
                    pos_ms: r.get(9)?,
                    cue_start_ms: r.get(10)?,
                    cue_end_ms: r.get(11)?,
                },
                created_at: r.get(12)?,
                anki_note_id: r.get(13)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn has_word(&self, word: &str, lang: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM vocab WHERE word_key=?1 AND lang=?2",
                params![word.to_lowercase(), lang],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn delete_word(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM vocab WHERE id=?1", [id])?;
        Ok(())
    }

    pub fn set_anki_id(&self, id: i64, note_id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE vocab SET anki_note_id=?2 WHERE id=?1",
            [id, note_id],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(w: &str) -> NewWord {
        NewWord {
            word: w.into(),
            lang: "en".into(),
            translation: "перевод".into(),
            sentence: "A sentence.".into(),
            media_title: "Movie".into(),
            media_path: "C:/movie.mkv".into(),
            pos_ms: 61_500,
            cue_start_ms: 61_000,
            cue_end_ms: 63_000,
            ..Default::default()
        }
    }

    #[test]
    fn translations_roundtrip_and_miss() {
        let mut s = Store::open_in_memory().unwrap();
        s.put_translations("google", "en", "ru", &[("Hi".into(), "Привет".into())])
            .unwrap();
        let got = s
            .get_translations("google", "en", "ru", &["Hi".into(), "Bye".into()])
            .unwrap();
        assert_eq!(got, vec![Some("Привет".into()), None]);
        assert_eq!(
            s.get_translations("deepl", "en", "ru", &["Hi".into()])
                .unwrap(),
            vec![None]
        );
        assert_eq!(
            s.get_translations("google", "en", "de", &["Hi".into()])
                .unwrap(),
            vec![None]
        );
    }

    #[test]
    fn transcript_roundtrip_and_overwrite() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.load_transcript("f:1", "w", "").unwrap(), None);
        let cues = vec![Cue {
            start: 0,
            end: 900,
            text: "He said \"hi\"\nok".into(),
        }];
        s.save_transcript("f:1", "w", "", 30_000, &[true, false], &cues)
            .unwrap();
        assert_eq!(
            s.load_transcript("f:1", "w", "").unwrap(),
            Some(Transcript {
                covered: 30_000,
                done: vec![true, false],
                cues: cues.clone()
            })
        );
        s.save_transcript("f:1", "w", "", 60_000, &[], &[]).unwrap();
        assert_eq!(
            s.load_transcript("f:1", "w", "").unwrap(),
            Some(Transcript {
                covered: 60_000,
                done: vec![],
                cues: vec![]
            })
        );
    }

    #[test]
    fn out_of_order_windows_are_kept() {
        let s = Store::open_in_memory().unwrap();
        let done = [false, false, true, true, false];
        s.save_transcript("f:2", "w", "en", 0, &done, &[]).unwrap();
        let got = s.load_transcript("f:2", "w", "en").unwrap().unwrap();
        assert_eq!(got.done, done);
        assert_eq!(got.covered, 0);
    }

    #[test]
    fn version_one_database_is_migrated() {
        let dir = std::env::temp_dir().join(format!("lp-store-v1-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("v1.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(&format!(
                "BEGIN; {} PRAGMA user_version = 1; COMMIT;
                 INSERT INTO transcript VALUES ('f:3', 'w', 'en', 45000, '[[0,900,\"old\"]]');",
                MIGRATIONS[0]
            ))
            .unwrap();
        }
        let s = Store::open(&path).unwrap();
        let got = s.load_transcript("f:3", "w", "en").unwrap().unwrap();
        assert_eq!(got.covered, 45_000);
        assert!(got.done.is_empty(), "old rows have no mask");
        assert_eq!(got.cues.len(), 1);
        s.save_transcript("f:3", "w", "en", 0, &[false, true], &got.cues)
            .unwrap();
        assert_eq!(
            s.load_transcript("f:3", "w", "en").unwrap().unwrap().done,
            [false, true]
        );
        drop(s);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn vocab_dedup_update_delete() {
        let s = Store::open_in_memory().unwrap();
        let id = s.save_word(&word("Apple")).unwrap();
        let mut again = word("apple");
        again.translation = String::new();
        again.sentence = "Another sentence.".into();
        assert_eq!(
            s.save_word(&again).unwrap(),
            id,
            "same word in another case is the same entry"
        );
        let other = s.save_word(&word("pear")).unwrap();
        assert_ne!(other, id);
        assert!(s.has_word("APPLE", "en").unwrap());
        assert!(!s.has_word("apple", "de").unwrap());

        let list = s.list_words().unwrap();
        assert_eq!(list.len(), 2);
        let apple = list.iter().find(|e| e.id == id).unwrap();
        assert_eq!(
            apple.word.translation, "перевод",
            "empty translation does not erase the old one"
        );
        assert_eq!(apple.word.sentence, "Another sentence.");
        assert_eq!(apple.word.pos_ms, 61_500);
        assert_eq!(apple.anki_note_id, None);

        s.set_anki_id(id, 777).unwrap();
        s.delete_word(other).unwrap();
        let list = s.list_words().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].anki_note_id, Some(777));
    }

    #[test]
    fn data_survives_reopen() {
        let dir = std::env::temp_dir().join(format!("lp-store-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        {
            let s = Store::open(&path).unwrap();
            s.save_word(&word("persist")).unwrap();
        }
        let s = Store::open(&path).unwrap();
        let list = s.list_words().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].word.sentence, "A sentence.");
        drop(s);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
