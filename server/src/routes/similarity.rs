use axum::{
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use crate::state::AppState;

#[derive(Deserialize)]
pub struct ValidateSimilarityBody {
    #[serde(default)]
    pub mensaje1: Option<String>,
    #[serde(default)]
    pub mensaje2: Option<String>,
    #[serde(default)]
    pub mensaje3: Option<String>,
    #[serde(rename = "umbralSimilitud", default = "default_umbral")]
    pub umbral_similitud: f64,
}

fn default_umbral() -> f64 {
    70.0
}

/// Fixes `"umbralSimilitud": 30,00` (Spanish decimal comma) → `"umbralSimilitud": 30.00`
/// before JSON parsing, so the body doesn't fail as invalid JSON.
fn fix_spanish_decimal(raw: &str) -> String {
    const KEY: &str = "\"umbralSimilitud\":";
    let Some(key_pos) = raw.find(KEY) else {
        return raw.to_string();
    };
    let after_key_pos = key_pos + KEY.len();
    let after_key = &raw[after_key_pos..];
    let ws_len = after_key.len() - after_key.trim_start_matches(|c: char| c.is_ascii_whitespace()).len();
    let num_pos = after_key_pos + ws_len;

    let num_bytes = raw[num_pos..].as_bytes();
    let mut i = 0;
    while i < num_bytes.len() && (num_bytes[i].is_ascii_digit() || num_bytes[i] == b',') {
        i += 1;
    }
    if i == 0 || !raw[num_pos..num_pos + i].contains(',') {
        return raw.to_string();
    }
    format!(
        "{}{}{}",
        &raw[..num_pos],
        raw[num_pos..num_pos + i].replace(',', "."),
        &raw[num_pos + i..]
    )
}

/// POST /api/validate-lexical-similarity  (no auth required)
pub async fn validate_lexical_similarity(
    State(state): State<AppState>,
    raw: axum::body::Bytes,
) -> Response {
    let raw_str = String::from_utf8_lossy(&raw);
    let fixed = fix_spanish_decimal(&raw_str);
    let body: ValidateSimilarityBody = match serde_json::from_str(&fixed) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("validate-lexical-similarity bad JSON: {e} — body: {raw_str}");
            return (StatusCode::BAD_REQUEST, format!("JSON inválido: {e}")).into_response();
        }
    };
    let umbral = body.umbral_similitud.clamp(0.0, 100.0);
    let api_key = state.config.openai_api_key.clone();
    let model = state.config.openai_model.clone();

    // Fill any missing messages via OpenAI (or return error if key not set)
    let (m1, m2, m3) = match fill_messages(body.mensaje1, body.mensaje2, body.mensaje3, &api_key, &model).await {
        Ok(msgs) => msgs,
        Err(e) => return error_xml(&format!("Error generando mensajes: {e}")),
    };

    let sim_12 = similarity_pct(&m1, &m2);
    let sim_13 = similarity_pct(&m1, &m3);
    let sim_23 = similarity_pct(&m2, &m3);

    let cumple_12 = sim_12 <= umbral;
    let cumple_13 = sim_13 <= umbral;
    let cumple_23 = sim_23 <= umbral;
    let cumple_all = cumple_12 && cumple_13 && cumple_23;

    // If any pair fails, try to rewrite via OpenAI; fall back to originals if unavailable.
    let recomendaciones = if !cumple_all {
        let rec = if umbral >= 30.0 && !api_key.is_empty() {
            match rewrite_messages(&m1, &m2, &m3, umbral, &api_key, &model).await {
                Ok(rec) => rec,
                Err(e) => {
                    tracing::warn!("rewrite_messages failed (using originals): {e}");
                    (m1.clone(), m2.clone(), m3.clone())
                }
            }
        } else {
            (m1.clone(), m2.clone(), m3.clone())
        };
        Some(rec)
    } else {
        None
    };

    // Re-analyse rewritten messages to populate SimilitudesNuevas and final CumpleValidacion.
    let (new_sim_12, new_sim_13, new_sim_23, cumple_final) = match &recomendaciones {
        Some((r1, r2, r3)) => {
            let s12 = similarity_pct(r1, r2);
            let s13 = similarity_pct(r1, r3);
            let s23 = similarity_pct(r2, r3);
            let cumple = cumple_all || (s12 <= umbral && s13 <= umbral && s23 <= umbral);
            (s12, s13, s23, cumple)
        }
        None => (sim_12, sim_13, sim_23, cumple_all),
    };

    let xml = build_xml(
        umbral,
        new_sim_12, new_sim_13, new_sim_23,
        cumple_final,
        recomendaciones.as_ref(),
    );

    (StatusCode::OK, [(header::CONTENT_TYPE, "text/xml; charset=utf-8")], xml).into_response()
}

// ── Similarity ────────────────────────────────────────────────────────────────

fn similarity_pct(a: &str, b: &str) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    strsim::normalized_levenshtein(a, b) * 100.0
}

// ── OpenAI calls ──────────────────────────────────────────────────────────────

async fn call_openai(api_key: &str, model: &str, prompt: &str) -> anyhow::Result<String> {
    use async_openai::{
        Client,
        config::OpenAIConfig,
        types::{ChatCompletionRequestUserMessageArgs, CreateChatCompletionRequestArgs},
    };

    let config = OpenAIConfig::new().with_api_key(api_key);
    let client = Client::with_config(config);

    let user_msg = ChatCompletionRequestUserMessageArgs::default()
        .content(prompt)
        .build()
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let request = CreateChatCompletionRequestArgs::default()
        .model(model)
        .messages(vec![user_msg.into()])
        .build()
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let response = client
        .chat()
        .create(request)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let content = response
        .choices
        .first()
        .and_then(|c| c.message.content.as_ref())
        .map(|s| s.clone())
        .unwrap_or_default();

    Ok(content)
}

async fn fill_messages(
    m1: Option<String>,
    m2: Option<String>,
    m3: Option<String>,
    api_key: &str,
    model: &str,
) -> anyhow::Result<(String, String, String)> {
    let m1 = m1.unwrap_or_default();
    let m2 = m2.unwrap_or_default();
    let m3 = m3.unwrap_or_default();

    let missing = [m1.is_empty(), m2.is_empty(), m3.is_empty()]
        .iter()
        .filter(|&&x| x)
        .count();

    if missing == 0 {
        return Ok((m1, m2, m3));
    }

    if api_key.is_empty() {
        return Err(anyhow::anyhow!(
            "Faltan mensajes y OPENAI_API_KEY no está configurado"
        ));
    }

    let existing: Vec<&str> = [m1.as_str(), m2.as_str(), m3.as_str()]
        .iter()
        .copied()
        .filter(|s| !s.is_empty())
        .collect();

    let prompt = format!(
        "Genera {missing} mensaje(s) de marketing para WhatsApp con diversidad léxica. \
        REGLAS IMPORTANTES: \
        1) El sistema ya antepone automáticamente un saludo y el nombre del socio, por lo que el mensaje \
        NO debe comenzar con ningún saludo ni referencia al destinatario (nada de 'Estimado socio', \
        'Hola socio', 'Le informamos', ni similares al inicio). Comenzá directamente con el contenido. \
        2) Si dentro del cuerpo del mensaje necesitás referirte al destinatario, usá siempre 'socio' \
        (nunca cliente, miembro u otro término), porque es una cooperativa. \
        Mensajes existentes: {}. \
        Devuelve SOLO los mensajes faltantes, uno por línea, sin numeración ni explicaciones.",
        existing.join(" | ")
    );

    let response = call_openai(api_key, model, &prompt).await?;
    let mut lines = response.lines().filter(|l| !l.trim().is_empty());

    let m1_out = if m1.is_empty() {
        lines.next().unwrap_or("Mensaje 1").trim().to_string()
    } else {
        m1
    };
    let m2_out = if m2.is_empty() {
        lines.next().unwrap_or("Mensaje 2").trim().to_string()
    } else {
        m2
    };
    let m3_out = if m3.is_empty() {
        lines.next().unwrap_or("Mensaje 3").trim().to_string()
    } else {
        m3
    };

    Ok((m1_out, m2_out, m3_out))
}

async fn rewrite_messages(
    m1: &str,
    m2: &str,
    m3: &str,
    umbral: f64,
    api_key: &str,
    model: &str,
) -> anyhow::Result<(String, String, String)> {
    let prompt = format!(
        "Reescribe los siguientes 3 mensajes de marketing para WhatsApp de manera que la \
        similitud léxica entre cualquier par sea menor al {umbral:.0}%. \
        Mantén el mismo tema y tono pero usa vocabulario y estructura muy diferentes. \
        REGLAS IMPORTANTES: \
        1) El sistema ya antepone automáticamente un saludo y el nombre del socio, por lo que cada mensaje \
        NO debe comenzar con ningún saludo ni referencia al destinatario (nada de 'Estimado socio', \
        'Hola socio', 'Le informamos', ni similares al inicio). Comenzá directamente con el contenido. \
        2) Si dentro del cuerpo del mensaje necesitás referirte al destinatario, usá siempre 'socio' \
        (nunca cliente, miembro u otro término), porque es una cooperativa. \
        Mensajes originales:\n1. {m1}\n2. {m2}\n3. {m3}\n\
        Devuelve EXACTAMENTE 3 mensajes, uno por línea, sin numeración ni explicaciones."
    );

    let response = call_openai(api_key, model, &prompt).await?;
    let mut lines = response.lines().filter(|l| !l.trim().is_empty()).take(3);

    let r1 = lines.next().unwrap_or(m1).trim().to_string();
    let r2 = lines.next().unwrap_or(m2).trim().to_string();
    let r3 = lines.next().unwrap_or(m3).trim().to_string();

    Ok((r1, r2, r3))
}

// ── XML builders ──────────────────────────────────────────────────────────────

fn build_xml(
    umbral: f64,
    sim_12: f64,
    sim_13: f64,
    sim_23: f64,
    cumple_all: bool,
    recomendaciones: Option<&(String, String, String)>,
) -> String {
    let cumple_validacion = if cumple_all { "true" } else { "false" };

    let (r1, r2, r3) = recomendaciones
        .map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str()))
        .unwrap_or(("", "", ""));

    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<ValidacionSimilitud xmlns=\"Nucleo\">\n\
  <Estado>SUCCESS</Estado>\n\
  <Mensaje>Validacion completada</Mensaje>\n\
  <Resultado>\n\
    <CumpleValidacion>{cumple_validacion}</CumpleValidacion>\n\
    <UmbralConfigurado>{umbral:.0}</UmbralConfigurado>\n\
    <Comparaciones/>\n\
    <Recomendaciones>\n\
      <Mensaje1Reescrito>{}</Mensaje1Reescrito>\n\
      <Mensaje2Reescrito>{}</Mensaje2Reescrito>\n\
      <Mensaje3Reescrito>{}</Mensaje3Reescrito>\n\
      <SimilitudesNuevas>\n\
        <Similitud1vs2>{sim_12:.1}</Similitud1vs2>\n\
        <Similitud1vs3>{sim_13:.1}</Similitud1vs3>\n\
        <Similitud2vs3>{sim_23:.1}</Similitud2vs3>\n\
      </SimilitudesNuevas>\n\
    </Recomendaciones>\n\
  </Resultado>\n\
</ValidacionSimilitud>",
        xml_escape(r1),
        xml_escape(r2),
        xml_escape(r3),
    )
}

fn error_xml(msg: &str) -> Response {
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<ValidacionSimilitud xmlns=\"Nucleo\">\
<Estado>ERROR</Estado>\
<Mensaje>{}</Mensaje>\
</ValidacionSimilitud>",
        xml_escape(msg)
    );
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        [(header::CONTENT_TYPE, "text/xml; charset=utf-8")],
        xml,
    )
        .into_response()
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
