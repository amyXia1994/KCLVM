use crossbeam_channel::after;
use crossbeam_channel::select;
use indexmap::IndexSet;
use lsp_server::Response;
use lsp_types::notification::Exit;
use lsp_types::request::GotoTypeDefinitionResponse;
use lsp_types::CompletionContext;
use lsp_types::CompletionItem;
use lsp_types::CompletionParams;
use lsp_types::CompletionResponse;
use lsp_types::CompletionTriggerKind;
use lsp_types::DocumentFormattingParams;
use lsp_types::GotoDefinitionParams;
use lsp_types::GotoDefinitionResponse;
use lsp_types::Hover;
use lsp_types::HoverContents;
use lsp_types::HoverParams;
use lsp_types::MarkedString;
use lsp_types::PublishDiagnosticsParams;
use lsp_types::ReferenceContext;
use lsp_types::ReferenceParams;
use lsp_types::TextDocumentIdentifier;
use lsp_types::TextDocumentItem;
use lsp_types::TextDocumentPositionParams;
use lsp_types::TextEdit;
use serde::Serialize;
use std::cell::Cell;
use std::cell::RefCell;
use std::env;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use kclvm_ast::ast::Program;
use kclvm_error::Diagnostic as KCLDiagnostic;
use kclvm_error::Position as KCLPos;
use kclvm_sema::resolver::scope::ProgramScope;

use lsp_types::Diagnostic;
use lsp_types::DiagnosticRelatedInformation;
use lsp_types::DiagnosticSeverity;
use lsp_types::Location;
use lsp_types::NumberOrString;
use lsp_types::Url;
use lsp_types::{Position, Range, TextDocumentContentChangeEvent};
use parking_lot::RwLock;
use proc_macro_crate::bench_test;

use lsp_server::{Connection, Message, Notification, Request};

use crate::completion::completion;
use crate::completion::into_completion_items;
use crate::completion::KCLCompletionItem;
use crate::config::Config;
use crate::from_lsp::file_path_from_url;

use crate::hover::hover;
use crate::main_loop::main_loop;
use crate::to_lsp::kcl_diag_to_lsp_diags;
use crate::util::to_json;
use crate::{
    goto_def::goto_definition,
    util::{apply_document_changes, parse_param_and_compile, Param},
};

pub(crate) fn compare_goto_res(
    res: Option<GotoTypeDefinitionResponse>,
    pos: (&String, u32, u32, u32, u32),
) {
    match res.unwrap() {
        lsp_types::GotoDefinitionResponse::Scalar(loc) => {
            let got_path = loc.uri.path();
            assert_eq!(got_path, pos.0);

            let (got_start, got_end) = (loc.range.start, loc.range.end);

            let expected_start = Position {
                line: pos.1, // zero-based
                character: pos.2,
            };

            let expected_end = Position {
                line: pos.3, // zero-based
                character: pos.4,
            };
            assert_eq!(got_start, expected_start);
            assert_eq!(got_end, expected_end);
        }
        _ => {
            unreachable!("test error")
        }
    }
}

pub(crate) fn compile_test_file(
    testfile: &str,
) -> (String, Program, ProgramScope, IndexSet<KCLDiagnostic>) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut test_file = path;
    test_file.push(testfile);

    let file = test_file.to_str().unwrap().to_string();

    let (program, prog_scope, diags) = parse_param_and_compile(
        Param { file: file.clone() },
        Some(Arc::new(RwLock::new(Default::default()))),
    )
    .unwrap();
    (file, program, prog_scope, diags)
}

fn build_lsp_diag(
    pos: (u32, u32, u32, u32),
    message: String,
    severity: Option<DiagnosticSeverity>,
    related_info: Vec<(String, (u32, u32, u32, u32), String)>,
    code: Option<NumberOrString>,
) -> Diagnostic {
    let related_information = if related_info.is_empty() {
        None
    } else {
        Some(
            related_info
                .iter()
                .map(|(file, pos, msg)| DiagnosticRelatedInformation {
                    location: Location {
                        uri: Url::from_file_path(file).unwrap(),
                        range: Range {
                            start: Position {
                                line: pos.0,
                                character: pos.1,
                            },
                            end: Position {
                                line: pos.2,
                                character: pos.3,
                            },
                        },
                    },
                    message: msg.clone(),
                })
                .collect(),
        )
    };
    Diagnostic {
        range: lsp_types::Range {
            start: Position {
                line: pos.0,
                character: pos.1,
            },
            end: Position {
                line: pos.2,
                character: pos.3,
            },
        },
        severity,
        code,
        code_description: None,
        source: None,
        message,
        related_information,
        tags: None,
        data: None,
    }
}

fn build_expect_diags() -> Vec<Diagnostic> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut test_file = path.clone();
    test_file.push("src/test_data/diagnostics.k");
    let file = test_file.to_str().unwrap();
    let expected_diags: Vec<Diagnostic> = vec![
        build_lsp_diag(
            (1, 4, 1, 4),
            "expected one of [\"identifier\", \"literal\", \"(\", \"[\", \"{\"] got newline"
                .to_string(),
            Some(DiagnosticSeverity::ERROR),
            vec![],
            Some(NumberOrString::String("InvalidSyntax".to_string())),
        ),
        build_lsp_diag(
            (0, 0, 0, 10),
            "pkgpath abc not found in the program".to_string(),
            Some(DiagnosticSeverity::ERROR),
            vec![],
            Some(NumberOrString::String("CannotFindModule".to_string())),
        ),
        build_lsp_diag(
            (0, 0, 0, 10),
            format!(
                "Cannot find the module abc from {}/src/test_data/abc",
                path.to_str().unwrap()
            ),
            Some(DiagnosticSeverity::ERROR),
            vec![],
            Some(NumberOrString::String("CannotFindModule".to_string())),
        ),
        build_lsp_diag(
            (8, 0, 8, 1),
            "Can not change the value of 'd', because it was declared immutable".to_string(),
            Some(DiagnosticSeverity::ERROR),
            vec![(
                file.to_string(),
                (7, 0, 7, 1),
                "The variable 'd' is declared here".to_string(),
            )],
            Some(NumberOrString::String("ImmutableError".to_string())),
        ),
        build_lsp_diag(
            (7, 0, 7, 1),
            "The variable 'd' is declared here".to_string(),
            Some(DiagnosticSeverity::ERROR),
            vec![(
                file.to_string(),
                (8, 0, 8, 1),
                "Can not change the value of 'd', because it was declared immutable".to_string(),
            )],
            Some(NumberOrString::String("ImmutableError".to_string())),
        ),
        build_lsp_diag(
            (2, 0, 2, 1),
            "expected str, got int(1)".to_string(),
            Some(DiagnosticSeverity::ERROR),
            vec![],
            Some(NumberOrString::String("TypeError".to_string())),
        ),
        build_lsp_diag(
            (0, 0, 0, 10),
            "Module 'abc' imported but unused".to_string(),
            Some(DiagnosticSeverity::WARNING),
            vec![],
            Some(NumberOrString::String("UnusedImportWarning".to_string())),
        ),
    ];
    expected_diags
}

#[test]
#[bench_test]
fn diagnostics_test() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut test_file = path.clone();
    test_file.push("src/test_data/diagnostics.k");
    let file = test_file.to_str().unwrap();

    let (_, _, diags) = parse_param_and_compile(
        Param {
            file: file.to_string(),
        },
        Some(Arc::new(RwLock::new(Default::default()))),
    )
    .unwrap();

    let diagnostics = diags
        .iter()
        .flat_map(|diag| kcl_diag_to_lsp_diags(diag, file))
        .collect::<Vec<Diagnostic>>();

    let expected_diags: Vec<Diagnostic> = build_expect_diags();
    for (get, expected) in diagnostics.iter().zip(expected_diags.iter()) {
        assert_eq!(get, expected)
    }
}

#[test]
#[bench_test]
fn test_apply_document_changes() {
    macro_rules! change {
        [$($sl:expr, $sc:expr; $el:expr, $ec:expr => $text:expr),+] => {
            vec![$(TextDocumentContentChangeEvent {
                range: Some(Range {
                    start: Position { line: $sl, character: $sc },
                    end: Position { line: $el, character: $ec },
                }),
                range_length: None,
                text: String::from($text),
            }),+]
        };
    }

    let mut text = String::new();
    apply_document_changes(&mut text, vec![]);
    assert_eq!(text, "");

    // Test if full updates work (without a range)
    apply_document_changes(
        &mut text,
        vec![TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: String::from("the"),
        }],
    );

    assert_eq!(text, "the");
    apply_document_changes(&mut text, change![0, 3; 0, 3 => " quick"]);
    assert_eq!(text, "the quick");

    apply_document_changes(&mut text, change![0, 0; 0, 4 => "", 0, 5; 0, 5 => " foxes"]);
    assert_eq!(text, "quick foxes");

    apply_document_changes(&mut text, change![0, 11; 0, 11 => "\ndream"]);
    assert_eq!(text, "quick foxes\ndream");

    apply_document_changes(&mut text, change![1, 0; 1, 0 => "have "]);
    assert_eq!(text, "quick foxes\nhave dream");

    apply_document_changes(
        &mut text,
        change![0, 0; 0, 0 => "the ", 1, 4; 1, 4 => " quiet", 1, 16; 1, 16 => "s\n"],
    );
    assert_eq!(text, "the quick foxes\nhave quiet dreams\n");

    apply_document_changes(
        &mut text,
        change![0, 15; 0, 15 => "\n", 2, 17; 2, 17 => "\n"],
    );
    assert_eq!(text, "the quick foxes\n\nhave quiet dreams\n\n");

    apply_document_changes(
        &mut text,
        change![1, 0; 1, 0 => "DREAM", 2, 0; 2, 0 => "they ", 3, 0; 3, 0 => "DON'T THEY?"],
    );
    assert_eq!(
        text,
        "the quick foxes\nDREAM\nthey have quiet dreams\nDON'T THEY?\n"
    );

    apply_document_changes(&mut text, change![0, 10; 1, 5 => "", 2, 0; 2, 12 => ""]);
    assert_eq!(text, "the quick \nthey have quiet dreams\n");

    text = String::from("❤️");
    apply_document_changes(&mut text, change![0, 0; 0, 0 => "a"]);
    assert_eq!(text, "a❤️");

    // todo: Non-ASCII char
    // text = String::from("a\nb");
    // apply_document_changes(&mut text, change![0, 1; 1, 0 => "\nțc", 0, 1; 1, 1 => "d"]);
    // assert_eq!(text, "adcb");

    // text = String::from("a\nb");
    // apply_document_changes(&mut text, change![0, 1; 1, 0 => "ț\nc", 0, 2; 0, 2 => "c"]);
    // assert_eq!(text, "ațc\ncb");
}

#[test]
#[bench_test]
fn file_path_from_url_test() {
    if cfg!(windows) {
        let url =
            Url::parse("file:///c%3A/Users/abc/Desktop/%E4%B8%AD%E6%96%87/ab%20c/abc.k").unwrap();
        let path = file_path_from_url(&url).unwrap();
        assert_eq!(path, "c:\\Users\\abc\\Desktop\\中文\\ab c\\abc.k");
    } else {
        let url = Url::parse("file:///Users/abc/Desktop/%E4%B8%AD%E6%96%87/ab%20c/abc.k").unwrap();
        let path = file_path_from_url(&url).unwrap();
        assert_eq!(path, "/Users/abc/Desktop/中文/ab c/abc.k");
    }
}

#[test]
fn goto_import_external_file_test() {
    let path = PathBuf::from(".")
        .join("src")
        .join("test_data")
        .join("goto_import_def_test")
        .join("main.k")
        .canonicalize()
        .unwrap()
        .display()
        .to_string();

    let _ = Command::new("kpm")
        .arg("metadata")
        .arg("--update")
        .current_dir(
            PathBuf::from(".")
                .join("src")
                .join("test_data")
                .join("goto_import_def_test")
                .canonicalize()
                .unwrap()
                .display()
                .to_string(),
        )
        .output()
        .unwrap();

    let (program, prog_scope, diags) = parse_param_and_compile(
        Param {
            file: path.to_string(),
        },
        Some(Arc::new(RwLock::new(Default::default()))),
    )
    .unwrap();

    assert_eq!(diags.len(), 0);

    // test goto import file: import .pkg.schema_def
    let pos = KCLPos {
        filename: path.to_string(),
        line: 1,
        column: Some(15),
    };
    let res = goto_definition(&program, &pos, &prog_scope);
    assert!(res.is_some());
}

// LSP e2e test

/// A `Project` represents a project that a language server can work with. Call the [`server`]
/// method to instantiate a language server that will serve information about the project.
pub struct Project {}

impl Project {
    /// Instantiates a language server for this project.
    pub fn server(self) -> Server {
        let config = Config::default();
        Server::new(config)
    }
}

/// An object that runs the language server main loop and enables sending and receiving messages
/// to and from it.
pub struct Server {
    next_request_id: Cell<i32>,
    worker: Option<std::thread::JoinHandle<()>>,
    client: Connection,
    messages: RefCell<Vec<Message>>,
}

impl Server {
    /// Constructs and initializes a new `Server`
    pub fn new(config: Config) -> Self {
        let (connection, client) = Connection::memory();

        let worker = std::thread::spawn(move || {
            main_loop(connection, config).unwrap();
        });

        Self {
            next_request_id: Cell::new(1),
            worker: Some(worker),
            client,
            messages: RefCell::new(Vec::new()),
        }
    }

    /// Sends a request to the language server, returning the response
    pub fn send_request<R: lsp_types::request::Request>(&self, params: R::Params) {
        let id = self.next_request_id.get();
        self.next_request_id.set(id.wrapping_add(1));
        let r = Request::new(id.into(), R::METHOD.to_string(), params);
        self.client.sender.send(r.into()).unwrap();
    }

    /// Sends an LSP notification to the main loop.
    pub(crate) fn notification<N: lsp_types::notification::Notification>(&self, params: N::Params)
    where
        N::Params: Serialize,
    {
        let r = Notification::new(N::METHOD.to_string(), params);
        self.send_notification(r)
    }

    /// Sends a server notification to the main loop
    fn send_notification(&self, not: Notification) {
        self.client.sender.send(Message::Notification(not)).unwrap();
    }

    /// A function to wait for a specific message to arrive
    fn wait_for_message_cond(&self, n: usize, cond: &dyn Fn(&Message) -> bool) {
        let mut total = 0;
        for msg in self.messages.borrow().iter() {
            if cond(msg) {
                total += 1
            }
        }
        while total < n {
            let msg = self.recv().expect("no response");
            if cond(&msg) {
                total += 1;
            }
        }
    }

    /// Receives a message from the message or timeout.
    pub(crate) fn recv(&self) -> Option<Message> {
        let timeout = Duration::from_secs(5);
        let msg = select! {
            recv(self.client.receiver) -> msg => msg.ok(),
            recv(after(timeout)) -> _ => panic!("timed out"),
        };
        if let Some(ref msg) = msg {
            self.messages.borrow_mut().push(msg.clone());
        }
        msg
    }

    /// Sends a request to the main loop and receives its response
    fn send_and_receive(&self, r: Request) -> Response {
        let id = r.id.clone();
        self.client.sender.send(r.into()).unwrap();
        while let Some(msg) = self.recv() {
            match msg {
                Message::Request(req) => {
                    panic!("did not expect a request as a response to a request: {req:?}")
                }
                Message::Notification(_) => (),
                Message::Response(res) => {
                    assert_eq!(res.id, id);
                    return res;
                }
            }
        }
        panic!("did not receive a response to our request");
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Send the proper shutdown sequence to ensure the main loop terminates properly
        self.notification::<Exit>(());

        // Cancel the main_loop
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

#[test]
fn notification_test() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut path = root.clone();

    path.push("src/test_data/diagnostics.k");

    let path = path.to_str().unwrap();
    let src = std::fs::read_to_string(path.clone()).unwrap();
    let server = Project {}.server();

    // Mock open file
    server.notification::<lsp_types::notification::DidOpenTextDocument>(
        lsp_types::DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: Url::from_file_path(path).unwrap(),
                language_id: "KCL".to_string(),
                version: 0,
                text: src,
            },
        },
    );

    // Wait for first "textDocument/publishDiagnostics" notification
    server.wait_for_message_cond(1, &|msg: &Message| match msg {
        Message::Notification(not) => not.method == "textDocument/publishDiagnostics",
        _ => false,
    });

    let msgs = server.messages.borrow();

    match msgs.last().unwrap() {
        Message::Notification(not) => {
            assert_eq!(
                not.params,
                to_json(PublishDiagnosticsParams {
                    uri: Url::from_file_path(path).unwrap(),
                    diagnostics: build_expect_diags(),
                    version: None,
                })
                .unwrap()
            );
        }
        _ => {
            unreachable!("test error")
        }
    }
}

#[test]
fn goto_def_test() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut path = root.clone();

    path.push("src/test_data/goto_def_test/goto_def.k");

    let path = path.to_str().unwrap();
    let src = std::fs::read_to_string(path.clone()).unwrap();
    let server = Project {}.server();

    // Mock open file
    server.notification::<lsp_types::notification::DidOpenTextDocument>(
        lsp_types::DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: Url::from_file_path(path).unwrap(),
                language_id: "KCL".to_string(),
                version: 0,
                text: src,
            },
        },
    );

    let id = server.next_request_id.get();
    server.next_request_id.set(id.wrapping_add(1));

    let r: Request = Request::new(
        id.into(),
        "textDocument/definition".to_string(),
        GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: Url::from_file_path(path).unwrap(),
                },
                position: Position::new(23, 9),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        },
    );

    // Send request and wait for it's response
    let res = server.send_and_receive(r);

    assert_eq!(
        res.result.unwrap(),
        to_json(GotoDefinitionResponse::Scalar(Location {
            uri: Url::from_file_path(path).unwrap(),
            range: Range {
                start: Position::new(20, 0),
                end: Position::new(23, 0),
            },
        }))
        .unwrap()
    );
}

#[test]
fn complete_test() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut path = root.clone();

    path.push("src/test_data/completion_test/dot/completion.k");

    let path = path.to_str().unwrap();
    let src = std::fs::read_to_string(path.clone()).unwrap();
    let server = Project {}.server();

    // Mock open file
    server.notification::<lsp_types::notification::DidOpenTextDocument>(
        lsp_types::DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: Url::from_file_path(path).unwrap(),
                language_id: "KCL".to_string(),
                version: 0,
                text: src,
            },
        },
    );

    let id = server.next_request_id.get();
    server.next_request_id.set(id.wrapping_add(1));

    let r: Request = Request::new(
        id.into(),
        "textDocument/completion".to_string(),
        CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: Url::from_file_path(path).unwrap(),
                },
                position: Position::new(11, 7),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: Some(CompletionContext {
                trigger_kind: CompletionTriggerKind::TRIGGER_CHARACTER,
                trigger_character: Some(".".to_string()),
            }),
        },
    );

    // Send request and wait for it's response
    let res = server.send_and_receive(r);

    assert_eq!(
        res.result.unwrap(),
        to_json(CompletionResponse::Array(vec![
            CompletionItem {
                label: "name".to_string(),
                ..Default::default()
            },
            CompletionItem {
                label: "age".to_string(),
                ..Default::default()
            }
        ]))
        .unwrap()
    )
}

#[test]
fn hover_test() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut path = root.clone();

    path.push("src/test_data/hover_test/hover.k");

    let path = path.to_str().unwrap();
    let src = std::fs::read_to_string(path.clone()).unwrap();
    let server = Project {}.server();

    // Mock open file
    server.notification::<lsp_types::notification::DidOpenTextDocument>(
        lsp_types::DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: Url::from_file_path(path).unwrap(),
                language_id: "KCL".to_string(),
                version: 0,
                text: src,
            },
        },
    );

    let id = server.next_request_id.get();
    server.next_request_id.set(id.wrapping_add(1));

    let r: Request = Request::new(
        id.into(),
        "textDocument/hover".to_string(),
        HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: Url::from_file_path(path).unwrap(),
                },
                position: Position::new(15, 7),
            },
            work_done_progress_params: Default::default(),
        },
    );

    // Send request and wait for it's response
    let res = server.send_and_receive(r);

    assert_eq!(
        res.result.unwrap(),
        to_json(Hover {
            contents: HoverContents::Array(vec![
                MarkedString::String("__main__\n\nschema Person".to_string()),
                MarkedString::String("hover doc test".to_string()),
                MarkedString::String(
                    "Attributes:\n\n__settings__?: {str:any}\n\nname: str\n\nage?: int".to_string()
                ),
            ]),
            range: None
        })
        .unwrap()
    )
}

#[test]
fn formatting_test() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut path = root.clone();

    path.push("src/test_data/format/format_range.k");

    let path = path.to_str().unwrap();
    let src = std::fs::read_to_string(path.clone()).unwrap();
    let server = Project {}.server();

    // Mock open file
    server.notification::<lsp_types::notification::DidOpenTextDocument>(
        lsp_types::DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: Url::from_file_path(path).unwrap(),
                language_id: "KCL".to_string(),
                version: 0,
                text: src,
            },
        },
    );

    let id = server.next_request_id.get();
    server.next_request_id.set(id.wrapping_add(1));

    let r: Request = Request::new(
        id.into(),
        "textDocument/formatting".to_string(),
        DocumentFormattingParams {
            text_document: TextDocumentIdentifier {
                uri: Url::from_file_path(path).unwrap(),
            },
            options: Default::default(),
            work_done_progress_params: Default::default(),
        },
    );

    // Send request and wait for it's response
    let res = server.send_and_receive(r);

    assert_eq!(
        res.result.unwrap(),
        to_json(Some(vec![TextEdit {
            range: Range::new(Position::new(0, 0), Position::new(u32::MAX, u32::MAX),),
            new_text: "a = 1\nb = 2\nc = 3\nd = 4\n".to_string()
        }]))
        .unwrap()
    )
}

// Integration testing of lsp and konfig
fn konfig_path() -> PathBuf {
    let konfig_path = Path::new(".")
        .canonicalize()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("test")
        .join("integration")
        .join("konfig");
    konfig_path
}

#[test]
fn konfig_goto_def_test_base() {
    let konfig_path = konfig_path();
    let mut base_path = konfig_path.clone();
    base_path.push("appops/nginx-example/base/base.k");
    let base_path_str = base_path.to_str().unwrap().to_string();
    let (program, prog_scope, _) = parse_param_and_compile(
        Param {
            file: base_path_str.clone(),
        },
        Some(Arc::new(RwLock::new(Default::default()))),
    )
    .unwrap();

    // schema def
    let pos = KCLPos {
        filename: base_path_str.clone(),
        line: 7,
        column: Some(30),
    };
    let res = goto_definition(&program, &pos, &prog_scope);
    let mut expected_path = konfig_path.clone();
    expected_path.push("base/pkg/kusion_models/kube/frontend/server.k");
    compare_goto_res(
        res,
        (&expected_path.to_str().unwrap().to_string(), 12, 0, 142, 31),
    );

    // schema def
    let pos = KCLPos {
        filename: base_path_str.clone(),
        line: 9,
        column: Some(32),
    };
    let res = goto_definition(&program, &pos, &prog_scope);
    let mut expected_path = konfig_path.clone();
    expected_path.push("base/pkg/kusion_models/kube/frontend/container/container.k");
    compare_goto_res(
        res,
        (&expected_path.to_str().unwrap().to_string(), 5, 0, 80, 111),
    );

    // schema attr
    let pos = KCLPos {
        filename: base_path_str.clone(),
        line: 9,
        column: Some(9),
    };
    let res = goto_definition(&program, &pos, &prog_scope);
    let mut expected_path = konfig_path.clone();
    expected_path.push("base/pkg/kusion_models/kube/frontend/server.k");
    compare_goto_res(
        res,
        (
            &expected_path.to_str().unwrap().to_string(),
            115,
            4,
            115,
            17,
        ),
    );

    // schema attr
    let pos = KCLPos {
        filename: base_path_str.clone(),
        line: 10,
        column: Some(10),
    };
    let res = goto_definition(&program, &pos, &prog_scope);
    let mut expected_path = konfig_path.clone();
    expected_path.push("base/pkg/kusion_models/kube/frontend/container/container.k");
    compare_goto_res(
        res,
        (&expected_path.to_str().unwrap().to_string(), 69, 4, 69, 9),
    );

    // import pkg
    let pos = KCLPos {
        filename: base_path_str.clone(),
        line: 2,
        column: Some(49),
    };
    let res = goto_definition(&program, &pos, &prog_scope);
    let mut expected_path = konfig_path.clone();
    expected_path.push("base/pkg/kusion_models/kube/frontend/service/service.k");
    compare_goto_res(
        res,
        (&expected_path.to_str().unwrap().to_string(), 0, 0, 0, 0),
    );
}

#[test]
fn konfig_goto_def_test_main() {
    let konfig_path = konfig_path();
    let mut main_path = konfig_path.clone();
    main_path.push("appops/nginx-example/dev/main.k");
    let main_path_str = main_path.to_str().unwrap().to_string();
    let (program, prog_scope, _) = parse_param_and_compile(
        Param {
            file: main_path_str.clone(),
        },
        Some(Arc::new(RwLock::new(Default::default()))),
    )
    .unwrap();

    // schema def
    let pos = KCLPos {
        filename: main_path_str.clone(),
        line: 6,
        column: Some(31),
    };
    let res = goto_definition(&program, &pos, &prog_scope);
    let mut expected_path = konfig_path.clone();
    expected_path.push("base/pkg/kusion_models/kube/frontend/server.k");
    compare_goto_res(
        res,
        (&expected_path.to_str().unwrap().to_string(), 12, 0, 142, 31),
    );

    // schema attr
    let pos = KCLPos {
        filename: main_path_str.clone(),
        line: 7,
        column: Some(14),
    };
    let res = goto_definition(&program, &pos, &prog_scope);
    let mut expected_path = konfig_path.clone();
    expected_path.push("base/pkg/kusion_models/kube/frontend/server.k");
    compare_goto_res(
        res,
        (
            &expected_path.to_str().unwrap().to_string(),
            112,
            4,
            112,
            22,
        ),
    );

    // import pkg
    let pos = KCLPos {
        filename: main_path_str.clone(),
        line: 2,
        column: Some(61),
    };
    let res = goto_definition(&program, &pos, &prog_scope);
    let mut expected_path = konfig_path.clone();
    expected_path.push("base/pkg/kusion_models/kube/templates/resource.k");
    compare_goto_res(
        res,
        (&expected_path.to_str().unwrap().to_string(), 0, 0, 0, 0),
    );
}

#[test]
fn konfig_completion_test_main() {
    let konfig_path = konfig_path();
    let mut main_path = konfig_path.clone();
    main_path.push("appops/nginx-example/dev/main.k");
    let main_path_str = main_path.to_str().unwrap().to_string();
    let (program, prog_scope, _) = parse_param_and_compile(
        Param {
            file: main_path_str.clone(),
        },
        Some(Arc::new(RwLock::new(Default::default()))),
    )
    .unwrap();

    // pkg's definition(schema) completion
    let pos = KCLPos {
        filename: main_path_str.clone(),
        line: 6,
        column: Some(27),
    };
    let got = completion(Some('.'), &program, &pos, &prog_scope).unwrap();
    let mut items: IndexSet<KCLCompletionItem> = IndexSet::new();
    items.insert(KCLCompletionItem {
        label: "Job".to_string(),
    });
    items.insert(KCLCompletionItem {
        label: "Server".to_string(),
    });

    let expect: CompletionResponse = into_completion_items(&items).into();
    assert_eq!(got, expect);
    items.clear();

    // schema attr completion
    let pos = KCLPos {
        filename: main_path_str.clone(),
        line: 7,
        column: Some(4),
    };
    let got = completion(None, &program, &pos, &prog_scope).unwrap();
    let attrs = [
        "frontend",
        "service",
        "container",
        "res_tpl",
        "rbac",
        "backend",
        "resource",
        "metadata",
        "apis",
        "corev1",
        "monitoringv1",
        "monitoringv1alpha1",
        "kubevelav1beta1",
        "commons",
        "vaultv1",
        "manifests",
        "__META_APP_NAME",
        "__META_ENV_TYPE_NAME",
        "__META_CLUSTER_NAME",
        "appConfiguration",
        "checkIdentical",
        "manifestsResourceMap",
        "remove_duplicated_iter",
        "__renderServerFrontendInstances__",
        "__renderServerBackendInstances__",
        "__renderJobFrontendInstances__",
        "__renderJobBackendInstances__",
        "__renderFrontendInstances__",
        "__renderBackendInstances__",
        "__rbac_map__",
        "__prometheus_map__",
        "__vault_map__",
        "__k8s__",
        "__array_of_resource_map___",
        "__resource_map_original___",
        "_providerResource",
        "_providerResourceMapping",
        "__resource_map___",
        "__is_kubevela_application__",
        "getId",
        "kubevela_app",
        "kubevela_output",
        "server_output",
        "__settings__",
        "name",
        "workloadType",
        "renderType",
        "replicas",
        "image",
        "schedulingStrategy",
        "mainContainer",
        "sidecarContainers",
        "initContainers",
        "useBuiltInLabels",
        "labels",
        "annotations",
        "useBuiltInSelector",
        "selector",
        "podMetadata",
        "volumes",
        "needNamespace",
        "enableMonitoring",
        "configMaps",
        "secrets",
        "services",
        "ingresses",
        "serviceAccount",
        "storage",
        "database",
    ];
    items.extend(attrs.iter().map(|item| KCLCompletionItem {
        label: item.to_string(),
    }));
    let expect: CompletionResponse = into_completion_items(&items).into();
    assert_eq!(got, expect);
    items.clear();

    // import path completion
    let pos = KCLPos {
        filename: main_path_str.clone(),
        line: 1,
        column: Some(35),
    };
    let got = completion(Some('.'), &program, &pos, &prog_scope).unwrap();
    let pkgs = [
        "common",
        "configmap",
        "container",
        "ingress",
        "job",
        "rbac",
        "resource",
        "secret",
        "server",
        "service",
        "serviceaccount",
        "sidecar",
        "storage",
        "strategy",
        "volume",
    ];
    items.extend(pkgs.iter().map(|item| KCLCompletionItem {
        label: item.to_string(),
    }));
    let expect: CompletionResponse = into_completion_items(&items).into();

    assert_eq!(got, expect);
}

#[test]
fn konfig_hover_test_main() {
    let konfig_path = konfig_path();
    let mut main_path = konfig_path.clone();
    main_path.push("appops/nginx-example/dev/main.k");
    let main_path_str = main_path.to_str().unwrap().to_string();
    let (program, prog_scope, _) = parse_param_and_compile(
        Param {
            file: main_path_str.clone(),
        },
        Some(Arc::new(RwLock::new(Default::default()))),
    )
    .unwrap();

    // schema def hover
    let pos = KCLPos {
        filename: main_path_str.clone(),
        line: 6,
        column: Some(32),
    };
    let got = hover(&program, &pos, &prog_scope).unwrap();
    match got.contents {
        HoverContents::Array(arr) => {
            let expect: Vec<MarkedString> = vec![
                "base.pkg.kusion_models.kube.frontend\n\nschema Server",
                "Server is abstaction of Deployment and StatefulSet.",
                "Attributes:\n\n__settings__?: {str:any}\n\nname?: str\n\nworkloadType: str(Deployment)|str(StatefulSet)\n\nrenderType?: str(Server)|str(KubeVelaApplication)\n\nreplicas: int\n\nimage: str\n\nschedulingStrategy: SchedulingStrategy\n\nmainContainer: Main\n\nsidecarContainers?: [Sidecar]\n\ninitContainers?: [Sidecar]\n\nuseBuiltInLabels?: bool\n\nlabels?: {str:str}\n\nannotations?: {str:str}\n\nuseBuiltInSelector?: bool\n\nselector?: {str:str}\n\npodMetadata?: ObjectMeta\n\nvolumes?: [Volume]\n\nneedNamespace?: bool\n\nenableMonitoring?: bool\n\nconfigMaps?: [ConfigMap]\n\nsecrets?: [Secret]\n\nservices?: [Service]\n\ningresses?: [Ingress]\n\nserviceAccount?: ServiceAccount\n\nstorage?: ObjectStorage\n\ndatabase?: DataBase"
            ]
            .iter()
            .map(|s| MarkedString::String(s.to_string()))
            .collect();
            assert_eq!(expect, arr);
        }
        _ => unreachable!("test error"),
    }

    // schema attr def hover
    let pos = KCLPos {
        filename: main_path_str.clone(),
        line: 7,
        column: Some(15),
    };
    let got = hover(&program, &pos, &prog_scope).unwrap();
    match got.contents {
        HoverContents::Array(arr) => {
            let expect: Vec<MarkedString> = vec![
                "schedulingStrategy: SchedulingStrategy",
                "SchedulingStrategy represents scheduling strategy.",
            ]
            .iter()
            .map(|s| MarkedString::String(s.to_string()))
            .collect();
            assert_eq!(expect, arr);
        }
        _ => unreachable!("test error"),
    }

    // variable hover
    let pos = KCLPos {
        filename: main_path_str.clone(),
        line: 6,
        column: Some(3),
    };
    let got = hover(&program, &pos, &prog_scope).unwrap();
    match got.contents {
        HoverContents::Scalar(s) => {
            assert_eq!(
                s,
                MarkedString::String("appConfiguration: Server".to_string())
            );
        }
        _ => unreachable!("test error"),
    }
}

#[test]
fn test_find_refs() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut path = root.clone();
    path.push("src/test_data/find_refs_test/main.k");

    let path = path.to_str().unwrap();
    let src = std::fs::read_to_string(path.clone()).unwrap();
    let server = Project {}.server();
    let url = Url::from_file_path(path).unwrap();

    // Mock open file
    server.notification::<lsp_types::notification::DidOpenTextDocument>(
        lsp_types::DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: url.clone(),
                language_id: "KCL".to_string(),
                version: 0,
                text: src,
            },
        },
    );

    let id = server.next_request_id.get();
    server.next_request_id.set(id.wrapping_add(1));

    let r: Request = Request::new(
        id.into(),
        "textDocument/references".to_string(),
        ReferenceParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: url.clone() },
                position: Position::new(0, 1),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: ReferenceContext {
                include_declaration: true,
            },
        },
    );

    // Send request and wait for it's response
    let res = server.send_and_receive(r);

    assert_eq!(
        res.result.unwrap(),
        to_json(vec![
            Location {
                uri: url.clone(),
                range: Range {
                    start: Position::new(0, 0),
                    end: Position::new(0, 1),
                },
            },
            Location {
                uri: url.clone(),
                range: Range {
                    start: Position::new(1, 4),
                    end: Position::new(1, 5),
                },
            },
            Location {
                uri: url.clone(),
                range: Range {
                    start: Position::new(2, 4),
                    end: Position::new(2, 5),
                },
            },
            Location {
                uri: url.clone(),
                range: Range {
                    start: Position::new(12, 14),
                    end: Position::new(12, 15),
                },
            },
        ])
        .unwrap()
    );
}
