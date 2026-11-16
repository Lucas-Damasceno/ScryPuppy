//! Evidence-grounded questions against the encrypted local capture library.

use chrono::{DateTime, Local};
use regex::Regex;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

use crate::library::{capture_base_from_row, hydrate_captures, CaptureDto};

#[derive(Clone, Debug, Default)]
pub struct AssistantSettings {
    pub language: String,
    pub provider: String,
    pub model: String,
    pub api_key: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ChatRequest {
    pub query: String,
    pub context_id: Option<String>,
    pub app: Option<String>,
    pub date_from: Option<String>,
    /// Exclusive upper bound for capture timestamps.
    pub date_to: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ChatAnswer {
    pub answer: String,
    pub confidence: String,
    pub evidence: Vec<EvidenceItem>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct EvidenceItem {
    pub capture_id: String,
    pub captured_at: String,
    pub context_names: Vec<String>,
    pub app_name: Option<String>,
    pub application_id: Option<String>,
    pub window_title: Option<String>,
    pub excerpt: String,
    pub matched_fields: Vec<String>,
}

#[derive(Clone, Debug)]
struct ScoredCapture {
    capture: CaptureDto,
    score: f64,
    matched_fields: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChatIntent {
    ApplicationId,
    WhereError,
    Related,
    OrganizeToday,
    SummarizeContext,
}

/// Searches only the requested local scope and optionally asks the configured AI
/// provider to summarize the selected evidence. Captures and queries are always
/// redacted at the provider boundary.
pub fn answer_chat(
    conn: &Connection,
    settings: &AssistantSettings,
    request: &ChatRequest,
) -> Result<ChatAnswer, String> {
    let portuguese = settings.language == "pt-BR";
    let query = request.query.trim();
    if query.is_empty() {
        return Ok(ChatAnswer {
            answer: if portuguese {
                "Digite uma pergunta para eu buscar nas capturas."
            } else {
                "Enter a question so I can search your captures."
            }
            .into(),
            confidence: "low".into(),
            evidence: Vec::new(),
        });
    }
    if query.chars().count() > 500 {
        return Err("A pergunta deve ter até 500 caracteres.".into());
    }

    let captures = load_chat_captures(conn, request)?;
    let intent = detect_chat_intent(query);
    let broad_query = is_broad_collection_query(query);
    let terms = query_terms(query);
    let mut scored = captures
        .into_iter()
        .enumerate()
        .map(|(index, capture)| score_capture(capture, &terms, intent, broad_query, index))
        .filter(|item| {
            item.score > 0.0
                || broad_query
                || matches!(
                    intent,
                    ChatIntent::OrganizeToday | ChatIntent::SummarizeContext
                )
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| right.capture.captured_at.cmp(&left.capture.captured_at))
            .then_with(|| left.capture.id.cmp(&right.capture.id))
    });
    let limit = request.limit.unwrap_or(6).clamp(1, 12);
    let mut answer = answer_from_evidence(query, intent, scored, limit, portuguese);

    if !settings.api_key.trim().is_empty() && !answer.evidence.is_empty() {
        match generate_ai_answer(settings, query, &answer, portuguese) {
            Ok(text) => {
                answer.answer = text;
                answer.confidence = "high".into();
            }
            Err(error) => {
                answer.answer = if portuguese {
                    format!("{}\n\nO provedor de IA falhou: {error}", answer.answer)
                } else {
                    format!("{}\n\nThe AI provider failed: {error}", answer.answer)
                };
            }
        }
    }
    Ok(answer)
}

fn load_chat_captures(conn: &Connection, request: &ChatRequest) -> Result<Vec<CaptureDto>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT c.id, c.content_text, c.captured_at,
                    c.source_app_name, c.source_app_id, c.source_process_id,
                    c.source_process_path, c.window_title, c.window_id, c.platform,
                    c.metadata_json, c.capture_kind
             FROM captures c
             WHERE (?1 IS NULL
                OR (?1 = 'inbox' AND c.capture_kind <> 'reference' AND NOT EXISTS (SELECT 1 FROM capture_contexts cc WHERE cc.capture_id = c.id))
                OR (?1 = 'knowledge-base' AND c.capture_kind = 'reference')
                OR EXISTS (SELECT 1 FROM capture_contexts cc WHERE cc.capture_id = c.id AND cc.context_id = ?1))
               AND (?2 IS NULL OR lower(coalesce(c.source_app_name, '')) LIKE '%' || lower(?2) || '%')
               AND (?3 IS NULL OR c.captured_at >= ?3)
               AND (?4 IS NULL OR c.captured_at < ?4)
             ORDER BY c.captured_at DESC, c.id
             LIMIT 500",
        )
        .map_err(|error| error.to_string())?;
    let mut captures = stmt
        .query_map(
            params![
                request.context_id.as_deref(),
                request.app.as_deref(),
                request.date_from.as_deref(),
                request.date_to.as_deref(),
            ],
            capture_base_from_row,
        )
        .map_err(|error| error.to_string())?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| error.to_string())?;
    hydrate_captures(conn, &mut captures)?;
    Ok(captures)
}

fn score_capture(
    capture: CaptureDto,
    terms: &[String],
    intent: ChatIntent,
    broad_query: bool,
    recency_index: usize,
) -> ScoredCapture {
    let mut score = 0.0;
    let mut matched_fields = Vec::new();
    let content = normalize(&capture.content_text);
    let app = normalize(capture.source_app_name.as_deref().unwrap_or_default());
    let app_id = normalize(capture.source_app_id.as_deref().unwrap_or_default());
    let window = normalize(capture.window_title.as_deref().unwrap_or_default());
    let contexts = normalize(
        &capture
            .contexts
            .iter()
            .map(|context| context.name.as_str())
            .collect::<Vec<_>>()
            .join(" "),
    );
    let tags = normalize(&capture.tags.join(" "));
    let entities = normalize(
        &capture
            .entities
            .iter()
            .map(|entity| format!("{} {}", entity.kind, entity.value))
            .collect::<Vec<_>>()
            .join(" "),
    );
    let ocr = normalize(
        capture
            .ocr
            .as_ref()
            .and_then(|ocr| ocr.text.as_deref())
            .unwrap_or_default(),
    );
    for term in terms {
        for (value, points, label) in [
            (&content, 3.0, "texto"),
            (&app, 5.0, "app"),
            (&app_id, 4.0, "application_id"),
            (&window, 3.0, "janela"),
            (&contexts, 3.0, "contexto"),
            (&tags, 2.0, "tags"),
            (&entities, 4.0, "entidades"),
            (&ocr, 4.0, "ocr"),
        ] {
            if value.contains(term) {
                score += points;
                if !matched_fields.iter().any(|existing| existing == label) {
                    matched_fields.push(label.to_string());
                }
            }
        }
    }
    if broad_query {
        score = 1.0 / (recency_index + 1) as f64;
        matched_fields.push("recency".into());
    }
    match intent {
        ChatIntent::ApplicationId if capture.source_app_id.is_some() => {
            score += 2.0;
            push_field(&mut matched_fields, "application_id");
        }
        ChatIntent::WhereError if capture.tags.iter().any(|tag| tag == "erro") => {
            score += 5.0;
            push_field(&mut matched_fields, "tags");
        }
        ChatIntent::OrganizeToday
            if capture_date_is(&capture.captured_at, Local::now().date_naive()) =>
        {
            score += 5.0;
            push_field(&mut matched_fields, "data");
        }
        _ => {}
    }
    ScoredCapture {
        capture,
        score,
        matched_fields,
    }
}

fn answer_from_evidence(
    query: &str,
    intent: ChatIntent,
    relevant: Vec<ScoredCapture>,
    limit: usize,
    portuguese: bool,
) -> ChatAnswer {
    match intent {
        ChatIntent::ApplicationId => answer_application_id(query, &relevant, portuguese),
        ChatIntent::WhereError => answer_where_error(&relevant, portuguese),
        ChatIntent::OrganizeToday => answer_organize_today(&relevant, portuguese),
        ChatIntent::SummarizeContext => answer_summary(query, &relevant, portuguese),
        ChatIntent::Related => answer_related(query, &relevant, limit, portuguese),
    }
}

fn answer_application_id(query: &str, relevant: &[ScoredCapture], portuguese: bool) -> ChatAnswer {
    let mut counts = std::collections::HashMap::<String, Vec<&ScoredCapture>>::new();
    for item in relevant {
        if let Some(app_id) = item
            .capture
            .source_app_id
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            counts.entry(app_id.into()).or_default().push(item);
        }
    }
    if let Some((app_id, items)) = counts.into_iter().max_by_key(|(_, items)| items.len()) {
        let evidence = items
            .into_iter()
            .take(5)
            .map(evidence_from_scored)
            .collect::<Vec<_>>();
        ChatAnswer {
            answer: if portuguese {
                format!(
                    "Application ID mais provável: `{app_id}`. Encontrei esse valor em {} captura(s).",
                    evidence.len()
                )
            } else {
                format!(
                    "Most likely Application ID: `{app_id}`. This value appears in {} capture(s).",
                    evidence.len()
                )
            },
            confidence: if evidence.len() >= 2 {
                "high"
            } else {
                "medium"
            }
            .into(),
            evidence,
        }
    } else {
        no_evidence_answer(query, portuguese)
    }
}

fn answer_where_error(relevant: &[ScoredCapture], portuguese: bool) -> ChatAnswer {
    let evidence = relevant
        .iter()
        .take(6)
        .map(evidence_from_scored)
        .collect::<Vec<_>>();
    let answer = if evidence.is_empty() {
        if portuguese {
            "Não encontrei evidências suficientes desse erro nas capturas."
        } else {
            "I could not find enough evidence of this error in your captures."
        }
        .into()
    } else if portuguese {
        format!("Encontrei {} captura(s) possivelmente relacionadas a esse erro. Abra as evidências para conferir o contexto original.", evidence.len())
    } else {
        format!("I found {} capture(s) that may be related to this error. Open the evidence to inspect the original context.", evidence.len())
    };
    ChatAnswer {
        answer,
        confidence: if evidence.len() >= 3 {
            "high"
        } else if evidence.is_empty() {
            "low"
        } else {
            "medium"
        }
        .into(),
        evidence,
    }
}

fn answer_related(
    query: &str,
    relevant: &[ScoredCapture],
    limit: usize,
    portuguese: bool,
) -> ChatAnswer {
    let evidence = relevant
        .iter()
        .take(limit)
        .map(evidence_from_scored)
        .collect::<Vec<_>>();
    let answer = if evidence.is_empty() {
        return no_evidence_answer(query, portuguese);
    } else if portuguese {
        format!(
            "Encontrei {} captura(s) relacionadas a \"{}\".",
            evidence.len(),
            query.trim()
        )
    } else {
        format!(
            "I found {} capture(s) related to \"{}\".",
            evidence.len(),
            query.trim()
        )
    };
    ChatAnswer {
        answer,
        confidence: if evidence.len() >= 4 {
            "high"
        } else {
            "medium"
        }
        .into(),
        evidence,
    }
}

fn answer_organize_today(relevant: &[ScoredCapture], portuguese: bool) -> ChatAnswer {
    let today = Local::now().date_naive();
    let today_items = relevant
        .iter()
        .filter(|item| capture_date_is(&item.capture.captured_at, today))
        .collect::<Vec<_>>();
    if today_items.is_empty() {
        return ChatAnswer {
            answer: if portuguese {
                "Não encontrei capturas de hoje para organizar."
            } else {
                "I could not find any captures from today to organize."
            }
            .into(),
            confidence: "low".into(),
            evidence: Vec::new(),
        };
    }
    let mut apps = std::collections::HashMap::<String, usize>::new();
    let mut contexts = std::collections::HashMap::<String, usize>::new();
    for item in &today_items {
        *apps
            .entry(item.capture.source_app_name.clone().unwrap_or_else(|| {
                if portuguese {
                    "Desconhecido"
                } else {
                    "Unknown"
                }
                .into()
            }))
            .or_default() += 1;
        if item.capture.contexts.is_empty() {
            *contexts.entry("Inbox".into()).or_default() += 1;
        } else {
            for context in &item.capture.contexts {
                *contexts.entry(context.name.clone()).or_default() += 1;
            }
        }
    }
    let evidence = today_items
        .into_iter()
        .take(8)
        .map(evidence_from_scored)
        .collect::<Vec<_>>();
    ChatAnswer {
        answer: if portuguese {
            format!(
                "Hoje encontrei {} captura(s). Principais apps: {}. Contextos mais usados: {}.",
                evidence.len(),
                top_counts(apps, 4),
                top_counts(contexts, 4)
            )
        } else {
            format!(
                "I found {} capture(s) from today. Top applications: {}. Most used contexts: {}.",
                evidence.len(),
                top_counts(apps, 4),
                top_counts(contexts, 4)
            )
        },
        confidence: "medium".into(),
        evidence,
    }
}

fn answer_summary(query: &str, relevant: &[ScoredCapture], portuguese: bool) -> ChatAnswer {
    let evidence = relevant
        .iter()
        .take(10)
        .map(evidence_from_scored)
        .collect::<Vec<_>>();
    if evidence.is_empty() {
        return no_evidence_answer(query, portuguese);
    }
    let mut tags = std::collections::HashMap::<String, usize>::new();
    let mut apps = std::collections::HashMap::<String, usize>::new();
    for item in relevant {
        for tag in &item.capture.tags {
            *tags.entry(tag.clone()).or_default() += 1;
        }
        *apps
            .entry(item.capture.source_app_name.clone().unwrap_or_else(|| {
                if portuguese {
                    "Desconhecido"
                } else {
                    "Unknown"
                }
                .into()
            }))
            .or_default() += 1;
    }
    ChatAnswer {
        answer: if portuguese {
            format!("Resumo local baseado em {} evidência(s): apps principais: {}; tags principais: {}.", evidence.len(), top_counts(apps, 4), top_counts(tags, 6))
        } else {
            format!(
                "Local summary based on {} evidence item(s): top applications: {}; top tags: {}.",
                evidence.len(),
                top_counts(apps, 4),
                top_counts(tags, 6)
            )
        },
        confidence: "medium".into(),
        evidence,
    }
}

fn no_evidence_answer(query: &str, portuguese: bool) -> ChatAnswer {
    ChatAnswer {
        answer: if portuguese {
            format!(
                "Não encontrei evidência suficiente para responder \"{}\" nas capturas atuais.",
                query.trim()
            )
        } else {
            format!(
                "I could not find enough evidence to answer \"{}\" in the current captures.",
                query.trim()
            )
        },
        confidence: "low".into(),
        evidence: Vec::new(),
    }
}

fn detect_chat_intent(query: &str) -> ChatIntent {
    let normalized = normalize(query);
    if normalized.contains("applicationid")
        || normalized.contains("application id")
        || normalized.contains("app id")
    {
        ChatIntent::ApplicationId
    } else if (normalized.contains("onde") || normalized.contains("where"))
        && (normalized.contains("erro") || normalized.contains("error"))
    {
        ChatIntent::WhereError
    } else if normalized.contains("organize") || normalized.contains("organizar") {
        ChatIntent::OrganizeToday
    } else if normalized.contains("resumo")
        || normalized.contains("resuma")
        || normalized.contains("summary")
        || normalized.contains("summarize")
    {
        ChatIntent::SummarizeContext
    } else {
        ChatIntent::Related
    }
}

fn evidence_from_scored(item: &ScoredCapture) -> EvidenceItem {
    EvidenceItem {
        capture_id: item.capture.id.clone(),
        captured_at: item.capture.captured_at.clone(),
        context_names: item
            .capture
            .contexts
            .iter()
            .map(|context| context.name.clone())
            .collect(),
        app_name: item.capture.source_app_name.clone(),
        application_id: item.capture.source_app_id.clone(),
        window_title: item.capture.window_title.clone(),
        excerpt: excerpt(&redact_sensitive_values(&item.capture.content_text)),
        matched_fields: item.matched_fields.clone(),
    }
}

fn generate_ai_answer(
    settings: &AssistantSettings,
    query: &str,
    answer: &ChatAnswer,
    portuguese: bool,
) -> Result<String, String> {
    let evidence = answer
        .evidence
        .iter()
        .enumerate()
        .map(|(index, item)| {
            format!(
                "Evidence {}:\n- capture_id: {}\n- time: {}\n- context: {}\n- app: {}\n- application_id: {}\n- window: {}\n- matched_fields: {}\n- excerpt: {}",
                index + 1,
                item.capture_id,
                item.captured_at,
                item.context_names.join(", "),
                item.app_name.as_deref().unwrap_or("unknown"),
                item.application_id.as_deref().unwrap_or("unknown"),
                item.window_title.as_deref().unwrap_or("unknown"),
                item.matched_fields.join(", "),
                item.excerpt,
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let (system, prompt) = if portuguese {
        (
            "Você é o assistente do ScryPuppy. Responda em português brasileiro. Use somente as evidências fornecidas. Cite as capturas usadas pelo horário, aplicativo e contexto. Se a evidência for insuficiente, diga isso claramente.",
            format!(
                "Pergunta do usuário:\n{}\n\nResposta local preliminar:\n{}\n\nEvidências locais:\n{}\n\nGere uma resposta final curta, precisa e apoiada pelas evidências.",
                redact_sensitive_values(query), answer.answer, evidence
            ),
        )
    } else {
        (
            "You are the ScryPuppy assistant. Respond in English. Use only the supplied evidence. Cite captures by time, application, and context. If evidence is insufficient, say so clearly.",
            format!(
                "User question:\n{}\n\nPreliminary local answer:\n{}\n\nLocal evidence:\n{}\n\nGenerate a short, precise answer supported by evidence.",
                redact_sensitive_values(query), answer.answer, evidence
            ),
        )
    };
    crate::ai::call_provider(
        &settings.provider,
        &settings.api_key,
        &settings.model,
        &redact_sensitive_values(system),
        &redact_sensitive_values(&prompt),
    )
}

/// Generates a source-grounded Markdown document from the already scoped chat evidence.
/// The provider is only called on this explicit action and receives redacted values.
pub fn generate_document_markdown(
    settings: &AssistantSettings,
    query: &str,
    answer: &ChatAnswer,
) -> Result<String, String> {
    if settings.api_key.trim().is_empty() {
        return Err("Configure uma chave de API antes de gerar documentos com IA.".into());
    }
    if answer.evidence.is_empty() {
        return Err("Não há fontes suficientes para gerar este documento.".into());
    }
    let portuguese = settings.language == "pt-BR";
    generate_document_markdown_with(query, answer, portuguese, |system, prompt| {
        crate::ai::call_provider(
            &settings.provider,
            &settings.api_key,
            &settings.model,
            system,
            prompt,
        )
    })
}

fn generate_document_markdown_with<F>(
    query: &str,
    answer: &ChatAnswer,
    portuguese: bool,
    mut call_provider: F,
) -> Result<String, String>
where
    F: FnMut(&str, &str) -> Result<String, String>,
{
    let evidence_batches = answer.evidence.chunks(6).collect::<Vec<_>>();
    let mut notes = Vec::with_capacity(evidence_batches.len());
    for (batch_index, batch) in evidence_batches.iter().enumerate() {
        let evidence = batch
            .iter()
            .enumerate()
            .map(|(index, item)| {
                format!(
                    "Source {} [capture:{}]\nDate: {}\nContext: {}\nApplication: {}\nWindow: {}\nExcerpt: {}",
                    batch_index * 6 + index + 1,
                    item.capture_id,
                    item.captured_at,
                    item.context_names.join(", "),
                    item.app_name.as_deref().unwrap_or("unknown"),
                    item.window_title.as_deref().unwrap_or("unknown"),
                    redact_sensitive_values(&item.excerpt),
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let (system, prompt) = if portuguese {
            (
                "Extraia fatos e relações úteis para um documento. Escreva em português brasileiro. Use apenas estas fontes e preserve as citações [capture:ID]. Não invente fatos.",
                format!(
                    "Pedido: {}\n\nLote {}/{} de evidências:\n{}",
                    redact_sensitive_values(query),
                    batch_index + 1,
                    evidence_batches.len(),
                    evidence
                ),
            )
        } else {
            (
                "Extract useful facts and relationships for a document. Use only these sources and preserve the [capture:ID] citations. Do not invent facts.",
                format!(
                    "Request: {}\n\nEvidence batch {}/{}:\n{}",
                    redact_sensitive_values(query),
                    batch_index + 1,
                    evidence_batches.len(),
                    evidence
                ),
            )
        };
        notes.push(call_provider(system, &redact_sensitive_values(&prompt))?);
    }

    let notes = notes.join("\n\n---\n\n");
    let (system, prompt) = if portuguese {
        (
            "Escreva em português brasileiro um documento Markdown claro, bem estruturado e útil. Use somente as notas e citações fornecidas. Não invente fatos. Identifique afirmações com [capture:ID] e termine com uma seção Fontes.",
            format!(
                "Pedido: {}\n\nSíntese preliminar: {}\n\nNotas extraídas das capturas locais:\n{}\n\nGere o documento final com título e subtítulos quando forem úteis.",
                redact_sensitive_values(query),
                redact_sensitive_values(&answer.answer),
                notes
            ),
        )
    } else {
        (
            "Write a clear, useful, well-structured Markdown document in English. Use only the supplied notes and citations. Do not invent facts. Mark claims with [capture:ID] references and end with a Sources section.",
            format!(
                "Request: {}\n\nPreliminary synthesis: {}\n\nNotes extracted from local captures:\n{}\n\nWrite the final document with a title and useful section headings.",
                redact_sensitive_values(query),
                redact_sensitive_values(&answer.answer),
                notes
            ),
        )
    };
    call_provider(system, &redact_sensitive_values(&prompt))
}

fn query_terms(query: &str) -> Vec<String> {
    const STOP_WORDS: &[&str] = &[
        "qual",
        "onde",
        "me",
        "mostre",
        "minhas",
        "meus",
        "das",
        "dos",
        "uma",
        "por",
        "para",
        "com",
        "que",
        "foi",
        "esse",
        "essa",
        "aplicacao",
        "aplicação",
        "app",
        "contexto",
        "capturas",
        "captura",
        "hoje",
        "resumo",
        "crie",
        "organize",
        "the",
        "and",
        "for",
        "with",
        "what",
        "where",
        "show",
        "find",
        "my",
        "from",
        "about",
        "a",
        "an",
        "in",
        "of",
        "on",
        "to",
        "use",
        "all",
        "everything",
        "entire",
        "whole",
        "tudo",
        "todo",
        "todos",
        "todas",
        "inteiro",
        "inteira",
        "biblioteca",
        "buscar",
        "busque",
        "resuma",
        "organizar",
        "organize",
    ];
    normalize(query)
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| term.chars().count() > 2 && !STOP_WORDS.contains(term))
        .map(str::to_string)
        .collect()
}

fn normalize(value: &str) -> String {
    value.to_lowercase()
}

fn is_broad_collection_query(query: &str) -> bool {
    const SCOPE_TERMS: &[&str] = &[
        "all",
        "everything",
        "entire",
        "whole",
        "tudo",
        "todo",
        "todos",
        "todas",
        "inteiro",
        "inteira",
    ];
    const NON_SUBJECT_TERMS: &[&str] = &[
        "a",
        "an",
        "and",
        "about",
        "capture",
        "captures",
        "clipboard",
        "condense",
        "context",
        "contexts",
        "copied",
        "create",
        "data",
        "document",
        "documents",
        "file",
        "files",
        "find",
        "from",
        "get",
        "i",
        "in",
        "include",
        "information",
        "items",
        "library",
        "list",
        "make",
        "memory",
        "me",
        "my",
        "notes",
        "of",
        "on",
        "organise",
        "organize",
        "our",
        "search",
        "show",
        "summarise",
        "summarize",
        "the",
        "to",
        "use",
        "arquivo",
        "arquivos",
        "as",
        "biblioteca",
        "buscar",
        "busque",
        "captura",
        "capturas",
        "condensar",
        "contexto",
        "contextos",
        "copiado",
        "copiada",
        "copiados",
        "copiadas",
        "copiei",
        "criar",
        "crie",
        "da",
        "das",
        "de",
        "do",
        "documento",
        "documentos",
        "dos",
        "e",
        "em",
        "encontrar",
        "encontre",
        "eu",
        "gerar",
        "gere",
        "informacao",
        "informação",
        "informacoes",
        "informações",
        "incluir",
        "inclua",
        "itens",
        "listar",
        "liste",
        "memoria",
        "memória",
        "meu",
        "meus",
        "minha",
        "minhas",
        "mostrar",
        "mostre",
        "na",
        "nas",
        "no",
        "nos",
        "nota",
        "notas",
        "o",
        "organizar",
        "os",
        "para",
        "procurar",
        "procure",
        "que",
        "resumir",
        "resuma",
        "sobre",
        "um",
        "uma",
        "usar",
    ];
    let normalized = normalize(query);
    let terms = normalized
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .collect::<Vec<_>>();
    terms.iter().any(|term| SCOPE_TERMS.contains(term))
        && terms
            .iter()
            .all(|term| SCOPE_TERMS.contains(term) || NON_SUBJECT_TERMS.contains(term))
}

fn push_field(fields: &mut Vec<String>, field: &str) {
    if !fields.iter().any(|existing| existing == field) {
        fields.push(field.into());
    }
}

fn capture_date_is(captured_at: &str, date: chrono::NaiveDate) -> bool {
    DateTime::parse_from_rfc3339(captured_at)
        .map(|value| value.with_timezone(&Local).date_naive() == date)
        .unwrap_or(false)
}

fn top_counts(counts: std::collections::HashMap<String, usize>, limit: usize) -> String {
    let mut items = counts.into_iter().collect::<Vec<_>>();
    items.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    let summary = items
        .into_iter()
        .take(limit)
        .map(|(name, count)| format!("{name} ({count})"))
        .collect::<Vec<_>>()
        .join(", ");
    if summary.is_empty() {
        "nenhum".into()
    } else {
        summary
    }
}

fn excerpt(text: &str) -> String {
    let clean = text.replace(['\n', '\r'], " ");
    let mut chars = clean.chars();
    let excerpt = chars.by_ref().take(220).collect::<String>();
    if chars.next().is_some() {
        format!("{excerpt}…")
    } else {
        excerpt
    }
}

fn redact_sensitive_values(text: &str) -> String {
    secret_pattern()
        .replace_all(text, "[redigido]")
        .into_owned()
}

fn secret_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"(?i)\b(?:sk-(?:proj-)?[a-z0-9_-]{12,}|gh[pousr]_[a-z0-9]{20,}|xox[baprs]-[a-z0-9-]{12,}|AIza[a-z0-9_-]{20,})\b")
            .expect("valid secret pattern")
    })
}

#[cfg(test)]
mod tests {
    use super::{
        answer_chat, generate_document_markdown, generate_document_markdown_with,
        AssistantSettings, ChatAnswer, ChatRequest, EvidenceItem,
    };
    use crate::{
        capture_store::save_clipboard_capture,
        clipboard::{ClipboardRepresentation, ClipboardSnapshot},
        schema::migrate,
    };
    use rusqlite::Connection;

    #[test]
    fn answers_from_the_selected_context_and_redacts_secret_excerpts() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        conn.execute(
            "INSERT INTO contexts (id, name, normalized_name, slug, created_at, updated_at)
             VALUES ('work', 'Trabalho', 'trabalho', 'trabalho', 'now', 'now')",
            [],
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let capture_id = save_clipboard_capture(
            &mut conn,
            dir.path(),
            &ClipboardSnapshot {
                representations: vec![ClipboardRepresentation::PlainText(
                    "Nota sobre planejamento sk-proj-abcdefghijklmnop".into(),
                )],
                formats: Vec::new(),
            },
            "capture",
        )
        .unwrap();
        save_clipboard_capture(
            &mut conn,
            dir.path(),
            &ClipboardSnapshot {
                representations: vec![ClipboardRepresentation::PlainText(
                    "Planejamento fora do Contexto selecionado".into(),
                )],
                formats: Vec::new(),
            },
            "capture",
        )
        .unwrap();
        let archived_id = save_clipboard_capture(
            &mut conn,
            dir.path(),
            &ClipboardSnapshot {
                representations: vec![ClipboardRepresentation::PlainText(
                    "Nota sobre planejamento arquivada".into(),
                )],
                formats: Vec::new(),
            },
            "capture",
        )
        .unwrap();
        let late_today_id = save_clipboard_capture(
            &mut conn,
            dir.path(),
            &ClipboardSnapshot {
                representations: vec![ClipboardRepresentation::PlainText(
                    "Planejamento registrado no fim do dia".into(),
                )],
                formats: Vec::new(),
            },
            "capture",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO capture_contexts (capture_id, context_id, assignment_origin, confidence, created_at)
             VALUES (?1, 'work', 'manual', 1, 'now')",
            [&capture_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO capture_contexts (capture_id, context_id, assignment_origin, confidence, created_at)
             VALUES (?1, 'work', 'manual', 1, 'now')",
            [&archived_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO capture_contexts (capture_id, context_id, assignment_origin, confidence, created_at)
             VALUES (?1, 'work', 'manual', 1, 'now')",
            [&late_today_id],
        )
        .unwrap();
        conn.execute(
            "UPDATE captures SET source_app_name = 'Visual Studio Code', captured_at = '2026-11-15T10:00:00Z'
             WHERE id = ?1",
            [&capture_id],
        )
        .unwrap();
        conn.execute(
            "UPDATE captures SET source_app_name = 'Visual Studio Code', captured_at = '2026-11-14T10:00:00Z'
             WHERE id = ?1",
            [&archived_id],
        )
        .unwrap();
        conn.execute(
            "UPDATE captures SET source_app_name = 'Visual Studio Code', captured_at = '2026-11-15T23:59:59Z'
             WHERE id = ?1",
            [&late_today_id],
        )
        .unwrap();

        let settings = AssistantSettings {
            language: "pt-BR".into(),
            ..AssistantSettings::default()
        };
        let answer = answer_chat(
            &conn,
            &settings,
            &ChatRequest {
                query: "planejamento".into(),
                context_id: Some("work".into()),
                app: None,
                date_from: None,
                date_to: None,
                limit: None,
            },
        )
        .unwrap();

        assert_eq!(answer.evidence.len(), 3);
        let original_evidence = answer
            .evidence
            .iter()
            .find(|evidence| evidence.capture_id == capture_id)
            .unwrap();
        assert!(answer.answer.contains("Encontrei"));
        assert_eq!(original_evidence.context_names, ["Trabalho"]);
        assert!(original_evidence.excerpt.contains("[redigido]"));
        assert!(!original_evidence.excerpt.contains("sk-proj-"));

        let filtered = answer_chat(
            &conn,
            &settings,
            &ChatRequest {
                query: "planejamento".into(),
                context_id: Some("work".into()),
                app: Some("Visual Studio".into()),
                date_from: Some("2026-11-15T00:00:00".into()),
                date_to: Some("2026-11-16T00:00:00".into()),
                limit: None,
            },
        )
        .unwrap();
        assert_eq!(filtered.evidence.len(), 2);
        assert!(filtered
            .evidence
            .iter()
            .any(|evidence| evidence.capture_id == late_today_id));

        let summary = answer_chat(
            &conn,
            &settings,
            &ChatRequest {
                query: "resuma todas as capturas".into(),
                context_id: Some("work".into()),
                app: None,
                date_from: None,
                date_to: None,
                limit: None,
            },
        )
        .unwrap();
        assert!(summary.answer.starts_with("Resumo local"));
        assert_eq!(summary.evidence.len(), 3);

        let daily = answer_chat(
            &conn,
            &settings,
            &ChatRequest {
                query: "organize as capturas de hoje".into(),
                context_id: Some("work".into()),
                app: None,
                date_from: None,
                date_to: None,
                limit: None,
            },
        )
        .unwrap();
        assert!(daily.answer.starts_with("Hoje encontrei 2 captura"));

        let no_match = answer_chat(
            &conn,
            &AssistantSettings {
                provider: "invalid-provider".into(),
                api_key: "must-not-be-used".into(),
                ..settings.clone()
            },
            &ChatRequest {
                query: "nothing-matches-this-term".into(),
                context_id: Some("work".into()),
                app: None,
                date_from: None,
                date_to: None,
                limit: None,
            },
        )
        .unwrap();
        assert!(no_match.evidence.is_empty());
        assert!(!no_match.answer.contains("provider failed"));
    }

    #[test]
    fn document_generation_requires_a_key_and_evidence_before_network_access() {
        let answer = ChatAnswer {
            answer: "Síntese local".into(),
            confidence: "low".into(),
            evidence: Vec::new(),
        };
        let settings = AssistantSettings {
            provider: "invalid-provider".into(),
            api_key: "configured".into(),
            ..AssistantSettings::default()
        };
        assert!(generate_document_markdown(&settings, "Pedido", &answer)
            .unwrap_err()
            .contains("fontes suficientes"));

        let no_key = AssistantSettings {
            api_key: String::new(),
            ..settings
        };
        assert!(generate_document_markdown(&no_key, "Pedido", &answer)
            .unwrap_err()
            .contains("chave de API"));
    }

    #[test]
    fn document_generation_batches_sources_and_redacts_provider_prompts() {
        let answer = ChatAnswer {
            answer: "Síntese com sk-proj-abcdefghijklmnop".into(),
            confidence: "high".into(),
            evidence: (0..7)
                .map(|index| EvidenceItem {
                    capture_id: format!("capture-{index}"),
                    captured_at: "2026-11-01".into(),
                    context_names: vec!["Trabalho".into()],
                    app_name: Some("Editor".into()),
                    application_id: None,
                    window_title: None,
                    excerpt: if index == 6 {
                        "Plano sk-proj-abcdefghijklmnop".into()
                    } else {
                        format!("Item {index}")
                    },
                    matched_fields: vec!["text".into()],
                })
                .collect(),
        };
        let mut prompts = Vec::new();
        let generated = generate_document_markdown_with(
            "Resumo sk-proj-abcdefghijklmnop",
            &answer,
            true,
            |system, prompt| {
                prompts.push((system.to_string(), prompt.to_string()));
                Ok(format!("Lote {}", prompts.len()))
            },
        )
        .unwrap();

        assert_eq!(generated, "Lote 3");
        assert_eq!(prompts.len(), 3);
        assert!(prompts[0].1.contains("capture:capture-0"));
        assert!(!prompts[0].1.contains("capture:capture-6"));
        assert!(prompts[1].1.contains("capture:capture-6"));
        assert!(prompts
            .iter()
            .all(|(_, prompt)| !prompt.contains("sk-proj-")));
        assert!(prompts
            .iter()
            .any(|(_, prompt)| prompt.contains("[redigido]")));
    }
}
