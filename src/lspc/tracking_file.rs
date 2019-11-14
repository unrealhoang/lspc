use lsp_types::{self as lsp};
use ropey::Rope;
use std::time::{Duration, Instant};
use url::Url;

enum SyncData {
    Incremental(lsp::DidChangeTextDocumentParams),
    Full(Rope),
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
            lsp::TextDocumentSyncKind::Full => SyncData::Full(Rope::new()),
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
                if content_change.range.is_none() {
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
                println!("Before sync content: {:?}", content);
                println!("Sync content change: {:?}", content_change);
                if content_change.range.is_none() {
                    let new_rope = Rope::from_str(&content_change.text);
                    std::mem::replace(content, new_rope);
                } else {
                    let start_line = content_change.range.unwrap().start.line as usize;
                    let end_line = content_change.range.unwrap().end.line as isize;
                    let end_line = end_line as usize;
                    let start_char = content.line_to_char(start_line);
                    let end_char = content.line_to_char(end_line);
                    content.remove(start_char..end_char);
                    content.insert(start_char, &content_change.text);
                }
                println!("After sync content: {:?}", content);
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
                        text: content.to_string(),
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
            range: None,
            range_length: None,
            text: "".to_owned(),
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
        assert_eq!("", sync_request.content_changes[0].text);

        // Two lines added
        // nvim_buf_lines_event[{buf}, {changedtick}, 0, 0, ["line1", "line2", "line3"], v:false]
        let change_event = lsp::TextDocumentContentChangeEvent {
            range: Some(lsp::Range {
                start: lsp::Position {
                    line: 0,
                    character: 0,
                },
                end: lsp::Position {
                    line: 0,
                    character: 0,
                },
            }),
            range_length: None,
            text: "line1\nline2\nline3".to_owned(),
        };
        tracking_file.track_change(6, &change_event);

        let sync_request = tracking_file.fetch_pending_changes().unwrap();

        assert_eq!(6, sync_request.text_document.version.unwrap());
        assert_eq!(1, sync_request.content_changes.len());
        assert_eq!("line1\nline2\nline3", sync_request.content_changes[0].text);

        // Remove two lines
        // nvim_buf_lines_event[{buf}, {changedtick}, 1, 3, [], v:false]
        let change_event = lsp::TextDocumentContentChangeEvent {
            range: Some(lsp::Range {
                start: lsp::Position {
                    line: 1,
                    character: 0,
                },
                end: lsp::Position {
                    line: 3,
                    character: 0,
                },
            }),
            range_length: None,
            text: "".to_owned(),
        };
        tracking_file.track_change(7, &change_event);

        let sync_request = tracking_file.fetch_pending_changes().unwrap();

        assert_eq!(7, sync_request.text_document.version.unwrap());
        assert_eq!(1, sync_request.content_changes.len());
        assert_eq!("line1\n", sync_request.content_changes[0].text);
    }
}
