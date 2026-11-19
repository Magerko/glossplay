use crate::{AiError, http};
use serde_json::{Value, json};
use std::path::Path;

pub const NOTE_TYPE: &str = "LinguaPlay";
const FIELDS: [&str; 8] = [
    "Word",
    "Translation",
    "Sentence",
    "SentenceTranslation",
    "Note",
    "Image",
    "Audio",
    "Source",
];

pub struct AnkiNote<'a> {
    pub deck: &'a str,
    pub word: &'a str,
    pub translation: &'a str,
    pub sentence: &'a str,
    pub sentence_translation: &'a str,
    pub note: &'a str,
    pub source: &'a str,
    pub image: Option<&'a Path>,
    pub audio: Option<&'a Path>,
}

pub struct AnkiClient {
    url: String,
}

impl AnkiClient {
    pub fn new(url: &str) -> Self {
        AnkiClient {
            url: url.to_owned(),
        }
    }

    fn call(&self, action: &str, params: Value) -> Result<Value, AiError> {
        let v = http::post_json(
            &self.url,
            &[],
            &json!({ "action": action, "version": 6, "params": params }),
        )?;
        match v["error"].as_str() {
            Some(e) if e.contains("duplicate") => Err(AiError::Duplicate),
            Some(e) => Err(AiError::BadResponse(format!("AnkiConnect: {e}"))),
            None => Ok(v["result"].clone()),
        }
    }

    pub fn version(&self) -> Result<u32, AiError> {
        let v = self.call("version", json!({}))?;
        v.as_u64()
            .map(|n| n as u32)
            .ok_or_else(|| AiError::BadResponse(v.to_string()))
    }

    /// Creates the deck and the "LinguaPlay" note type when they are missing.
    pub fn ensure_setup(&self, deck: &str) -> Result<(), AiError> {
        self.call("createDeck", json!({ "deck": deck }))?;
        let models = self.call("modelNames", json!({}))?;
        if models
            .as_array()
            .is_some_and(|m| m.iter().any(|n| n == NOTE_TYPE))
        {
            return Ok(());
        }
        self.call(
            "createModel",
            json!({
                "modelName": NOTE_TYPE,
                "inOrderFields": FIELDS,
                "css": ".card{font-family:sans-serif;font-size:22px;text-align:center}\
                        .word{font-size:34px;font-weight:600}.muted{color:#888;font-size:18px}\
                        img{max-width:100%;border-radius:8px}",
                "cardTemplates": [{
                    "Name": "Recognition",
                    "Front": "<div class=word>{{Word}}</div><div>{{Sentence}}</div>{{Audio}}",
                    "Back": "{{FrontSide}}<hr id=answer><div class=word>{{Translation}}</div>\
                             <div>{{SentenceTranslation}}</div><div class=muted>{{Note}}</div>\
                             {{Image}}<div class=muted>{{Source}}</div>",
                }],
            }),
        )?;
        Ok(())
    }

    /// Returns the new note id; a note with the same word already in the deck is `Duplicate`.
    pub fn add_note(&self, n: &AnkiNote) -> Result<i64, AiError> {
        let mut note = json!({
            "deckName": n.deck,
            "modelName": NOTE_TYPE,
            "fields": {
                "Word": n.word,
                "Translation": n.translation,
                "Sentence": n.sentence,
                "SentenceTranslation": n.sentence_translation,
                "Note": n.note,
                "Source": n.source,
            },
            "options": { "allowDuplicate": false, "duplicateScope": "deck" },
            "tags": ["linguaplay"],
        });
        if let Some(media) = media_entry(n.image, "Image") {
            note["picture"] = json!([media]);
        }
        if let Some(media) = media_entry(n.audio, "Audio") {
            note["audio"] = json!([media]);
        }
        let v = self.call("addNote", json!({ "note": note }))?;
        v.as_i64()
            .ok_or_else(|| AiError::BadResponse(format!("addNote returned {v}")))
    }
}

fn media_entry(path: Option<&Path>, field: &str) -> Option<Value> {
    let path = path?;
    let filename = path.file_name()?.to_string_lossy().into_owned();
    Some(json!({ "path": path.to_string_lossy(), "filename": filename, "fields": [field] }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testserver::TestServer;

    fn note<'a>(image: Option<&'a Path>, audio: Option<&'a Path>) -> AnkiNote<'a> {
        AnkiNote {
            deck: "LinguaPlay",
            word: "queer",
            translation: "странный",
            sentence: "They were indeed a queer-looking party.",
            sentence_translation: "Компания и правда была странная.",
            note: "",
            source: "Alice — 00:32",
            image,
            audio,
        }
    }

    #[test]
    fn add_note_sends_fields_and_media_then_reports_duplicate() {
        let srv = TestServer::start(vec![
            (200, r#"{"result":1496198395707,"error":null}"#.into()),
            (
                200,
                r#"{"result":null,"error":"cannot create note because it is a duplicate"}"#.into(),
            ),
        ]);
        let anki = AnkiClient::new(&srv.url);
        let (img, snd) = (Path::new("C:/tmp/lp_1.jpg"), Path::new("C:/tmp/lp_1.mp3"));
        assert_eq!(
            anki.add_note(&note(Some(img), Some(snd))).unwrap(),
            1496198395707
        );
        assert_eq!(anki.add_note(&note(None, None)), Err(AiError::Duplicate));

        let got = srv.received.lock().unwrap();
        let first: Value = serde_json::from_str(&got[0].body).unwrap();
        assert_eq!(first["action"], "addNote");
        assert_eq!(first["version"], 6);
        let n = &first["params"]["note"];
        assert_eq!(n["fields"]["Word"], "queer");
        assert_eq!(n["fields"]["Translation"], "странный");
        assert_eq!(
            n["fields"]["Sentence"],
            "They were indeed a queer-looking party."
        );
        assert_eq!(n["picture"][0]["filename"], "lp_1.jpg");
        assert_eq!(n["picture"][0]["fields"][0], "Image");
        assert_eq!(n["audio"][0]["path"], "C:/tmp/lp_1.mp3");
        assert_eq!(n["options"]["allowDuplicate"], false);
        let second: Value = serde_json::from_str(&got[1].body).unwrap();
        assert!(second["params"]["note"].get("picture").is_none());
    }

    #[test]
    fn setup_creates_model_only_when_missing() {
        let srv = TestServer::start(vec![
            (200, r#"{"result":1,"error":null}"#.into()),
            (200, r#"{"result":["Basic"],"error":null}"#.into()),
            (200, r#"{"result":{},"error":null}"#.into()),
        ]);
        AnkiClient::new(&srv.url)
            .ensure_setup("LinguaPlay")
            .unwrap();
        let got = srv.received.lock().unwrap();
        let actions: Vec<String> = got
            .iter()
            .map(|r| {
                serde_json::from_str::<Value>(&r.body).unwrap()["action"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(actions, ["createDeck", "modelNames", "createModel"]);

        let srv = TestServer::start(vec![
            (200, r#"{"result":1,"error":null}"#.into()),
            (
                200,
                r#"{"result":["Basic","LinguaPlay"],"error":null}"#.into(),
            ),
        ]);
        AnkiClient::new(&srv.url)
            .ensure_setup("LinguaPlay")
            .unwrap();
        assert_eq!(srv.count(), 2);
    }

    #[test]
    fn anki_not_running_is_a_network_error() {
        assert!(matches!(
            AnkiClient::new("http://127.0.0.1:1").version(),
            Err(AiError::Network(_))
        ));
    }
}
