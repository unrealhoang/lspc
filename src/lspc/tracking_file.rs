use lsp_types::{self as lsp};
use std::time::{Duration, Instant};
use url::Url;

enum SyncData {
    Incremental(lsp::DidChangeTextDocumentParams),
    Full(Vec<String>),
    None,
}

pub struct TrackingFile {
    pub handler_id: u64,
    pub sent_did_open: bool,
    pub scheduled_sync_at: Option<Instant>,
    version: i64,
    uri: Url,
    sync_data: SyncData,
}

impl TrackingFile {
    pub fn new(handler_id: u64, uri: Url, sync_kind: lsp::TextDocumentSyncKind) -> Self {
        let sync_data = match sync_kind {
            lsp::TextDocumentSyncKind::None => SyncData::None,
            lsp::TextDocumentSyncKind::Incremental => {
                SyncData::Incremental(lsp::DidChangeTextDocumentParams {
                    text_document: lsp::VersionedTextDocumentIdentifier {
                        uri: uri.clone(),
                        version: None,
                    },
                    content_changes: Vec::new(),
                })
            }
            lsp::TextDocumentSyncKind::Full => SyncData::Full(Vec::new()),
        };

        TrackingFile {
            handler_id,
            sent_did_open: false,
            scheduled_sync_at: None,
            version: 0,
            uri,
            sync_data,
        }
    }

    pub fn track_change(
        &mut self,
        version: i64,
        content_change: &lsp::TextDocumentContentChangeEvent,
    ) {
        self.version = version;
        match self.sync_data {
            SyncData::Incremental(ref mut changes) => {
                if content_change.range.unwrap().start.line as i64 == -1 {
                    return;
                }
                let last_content_change = changes.content_changes.iter_mut().last();
                if let Some(last_content_change) = last_content_change {
                    if last_content_change.range == content_change.range {
                        std::mem::replace(last_content_change, content_change.clone());
                    } else {
                        changes.content_changes.push(content_change.clone());
                    }
                } else {
                    changes.content_changes.push(content_change.clone());
                }
            }
            SyncData::Full(ref mut content) => {
                let mut start_line = content_change.range.unwrap().start.line as usize;
                let end_line = content_change.range.unwrap().end.line as isize;

                if end_line == -1 {
                    content.clear();
                    content.extend(content_change.text.split("\n").map(ToOwned::to_owned));
                } else {
                    let end_line = end_line as usize;
                    content.drain(start_line..end_line);
                    for new_line in content_change.text.split("\n").map(ToOwned::to_owned) {
                        content.insert(start_line, new_line);
                        start_line += 1;
                    }
                }
            }
            SyncData::None => {}
        }
    }

    pub fn fetch_pending_changes(&mut self) -> Option<lsp::DidChangeTextDocumentParams> {
        let mut sync_content = lsp::DidChangeTextDocumentParams {
            text_document: lsp::VersionedTextDocumentIdentifier {
                uri: self.uri.clone(),
                version: Some(self.version),
            },
            content_changes: Vec::new(),
        };

        self.scheduled_sync_at = None;
        match self.sync_data {
            SyncData::Incremental(ref mut cur_sync_content) => {
                std::mem::swap(cur_sync_content, &mut sync_content);
                if !sync_content.content_changes.is_empty() {
                    Some(sync_content)
                } else {
                    None
                }
            }
            SyncData::Full(ref mut content) => {
                sync_content
                    .content_changes
                    .push(lsp::TextDocumentContentChangeEvent {
                        range: None,
                        range_length: None,
                        // FIXME: use client config separator
                        text: content.join("\n"),
                    });
                Some(sync_content)
            }
            SyncData::None => None,
        }
    }

    pub fn delay_sync_in(&mut self, duration: Duration) {
        if let None = self.scheduled_sync_at {
            self.scheduled_sync_at = Some(Instant::now() + duration);
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn tracking_file_full() {
        #[cfg(not(target_os = "windows"))]
        let file_path = "/a/b/c/d";
        #[cfg(target_os = "windows")]
        let file_path = r#"C:\\a\b\d"#;

        let mut tracking_file = TrackingFile::new(
            1,
            Url::from_file_path(file_path).unwrap(),
            lsp::TextDocumentSyncKind::Full,
        );
        let change_event = lsp::TextDocumentContentChangeEvent {
            range: Some(lsp::Range {
                start: lsp::Position {
                    line: 0,
                    character: 0,
                },
                end: lsp::Position {
                    line: 18446744073709551615,
                    character: 0,
                },
            }),
            range_length: None,
            text: "1".to_owned(),
        };

        tracking_file.track_change(5, &change_event);
        let sync_request = tracking_file.fetch_pending_changes();

        assert_eq!(true, sync_request.is_some());

        let sync_request = sync_request.unwrap();
        assert_eq!(
            Url::from_file_path(file_path).unwrap(),
            sync_request.text_document.uri
        );
        assert_eq!(5, sync_request.text_document.version.unwrap());
        assert_eq!(1, sync_request.content_changes.len());
        assert_eq!("1", sync_request.content_changes[0].text);
    }
}
