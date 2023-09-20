use lsp_types::{
    ClientCapabilities, CodeActionKind, CodeActionOptions, CodeActionProviderCapability,
    CompletionOptions, HoverProviderCapability, OneOf, ServerCapabilities,
    TextDocumentSyncCapability, TextDocumentSyncKind, WorkDoneProgressOptions,
};

/// Returns the capabilities of this LSP server implementation given the capabilities of the client.
pub fn server_capabilities(client_caps: &ClientCapabilities) -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        document_symbol_provider: Some(OneOf::Left(true)),
        completion_provider: Some(CompletionOptions {
            resolve_provider: None,
            trigger_characters: Some(vec![String::from(".")]),
            all_commit_characters: None,
            work_done_progress_options: WorkDoneProgressOptions {
                work_done_progress: None,
            },
        }),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        code_action_provider: Some(
            client_caps
                .text_document
                .as_ref()
                .and_then(|it| it.code_action.as_ref())
                .and_then(|it| it.code_action_literal_support.as_ref())
                .map_or(CodeActionProviderCapability::Simple(true), |_| {
                    CodeActionProviderCapability::Options(CodeActionOptions {
                        // Advertise support for all built-in CodeActionKinds.
                        // Ideally we would base this off of the client capabilities
                        // but the client is supposed to fall back gracefully for unknown values.
                        code_action_kinds: Some(vec![CodeActionKind::QUICKFIX]),
                        resolve_provider: None,
                        work_done_progress_options: Default::default(),
                    })
                }),
        ),
        document_formatting_provider: Some(OneOf::Left(true)),
        document_range_formatting_provider: Some(OneOf::Left(true)),
        references_provider: Some(OneOf::Left(true)),
        ..Default::default()
    }
}
