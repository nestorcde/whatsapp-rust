use axum::{
    Json,
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

/// POST /api/validate-lexical-similarity  (no auth required)
pub async fn validate_lexical_similarity(
    State(state): State<AppState>,
    Json(body): Json<ValidateSimilarityBody>,
) -> Response {
    let umbral = body.umbral_similitud.clamp(0.0, 100.0);
    let api_key = state.config.openai_api_key.clone();

    // Fill any missing messages via OpenAI (or return error if key not set)
    let (m1, m2, m3) = match fill_messages(body.mensaje1, body.mensaje2, body.mensaje3, &api_key).await {
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

    // If any pair fails and the threshold is meaningful, ask OpenAI to rewrite
    let recomendaciones = if !cumple_all && umbral >= 30.0 && !api_key.is_empty() {
        match rewrite_messages(&m1, &m2, &m3, umbral, &api_key).await {
            Ok(rec) => Some(rec),
            Err(e) => {
                tracing::warn!("rewrite_messages failed: {e}");
                None
            }
        }
    } else {
        None
    };

    let xml = build_xml(
        umbral, &m1, &m2, &m3,
        sim_12, sim_13, sim_23,
        cumple_12, cumple_13, cumple_23,
        cumple_all,
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

async fn call_openai(api_key: &str, prompt: &str) -> anyhow::Result<String> {
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
        .model("gpt-4o-mini")
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
        Mensajes existentes: {}. \
        Devuelve SOLO los mensajes faltantes, uno por línea, sin numeración ni explicaciones.",
        existing.join(" | ")
    );

    let response = call_openai(api_key, &prompt).await?;
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
) -> anyhow::Result<(String, String, String)> {
    let prompt = format!(
        "Reescribe los siguientes 3 mensajes de marketing para WhatsApp de manera que la \
        similitud léxica entre cualquier par sea menor al {umbral:.0}%. \
        Mantén el mismo tema y tono pero usa vocabulario y estructura muy diferentes. \
        Mensajes originales:\n1. {m1}\n2. {m2}\n3. {m3}\n\
        Devuelve EXACTAMENTE 3 mensajes, uno por línea, sin numeración ni explicaciones."
    );

    let response = call_openai(api_key, &prompt).await?;
    let mut lines = response
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(3);

    let r1 = lines.next().unwrap_or(m1).trim().to_string();
    let r2 = lines.next().unwrap_or(m2).trim().to_string();
    let r3 = lines.next().unwrap_or(m3).trim().to_string();

    Ok((r1, r2, r3))
}

// ── XML builders ──────────────────────────────────────────────────────────────

fn build_xml(
    umbral: f64,
    m1: &str,
    m2: &str,
    m3: &str,
    sim_12: f64,
    sim_13: f64,
    sim_23: f64,
    cumple_12: bool,
    cumple_13: bool,
    cumple_23: bool,
    cumple_all: bool,
    recomendaciones: Option<&(String, String, String)>,
) -> String {
    let rec_cumple = recomendaciones.map_or(false, |(r1, r2, r3)| {
        similarity_pct(r1, r2) <= umbral
            && similarity_pct(r1, r3) <= umbral
            && similarity_pct(r2, r3) <= umbral
    });

    let cumple_final = cumple_all || rec_cumple;

    let rec_xml = recomendaciones.map_or(String::new(), |(r1, r2, r3)| {
        format!(
            "\n    <recomendaciones>\
            \n      <mensaje1>{}</mensaje1>\
            \n      <mensaje2>{}</mensaje2>\
            \n      <mensaje3>{}</mensaje3>\
            \n      <cumpleValidacion>{rec_cumple}</cumpleValidacion>\
            \n    </recomendaciones>",
            xml_escape(r1),
            xml_escape(r2),
            xml_escape(r3),
        )
    });

    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<response>\n\
  <status>SUCCESS</status>\n\
  <message>Validación completada</message>\n\
  <data>\n\
    <cumpleValidacion>{cumple_final}</cumpleValidacion>\n\
    <umbralConfigurado>{umbral:.0}</umbralConfigurado>\n\
    <mensajes>\n\
      <mensaje1>{}</mensaje1>\n\
      <mensaje2>{}</mensaje2>\n\
      <mensaje3>{}</mensaje3>\n\
    </mensajes>\n\
    <comparaciones>\n\
      <comparacion>\n\
        <mensajes>Mensaje 1 vs Mensaje 2</mensajes>\n\
        <similitud>{sim_12:.1}</similitud>\n\
        <cumpleUmbral>{cumple_12}</cumpleUmbral>\n\
      </comparacion>\n\
      <comparacion>\n\
        <mensajes>Mensaje 1 vs Mensaje 3</mensajes>\n\
        <similitud>{sim_13:.1}</similitud>\n\
        <cumpleUmbral>{cumple_13}</cumpleUmbral>\n\
      </comparacion>\n\
      <comparacion>\n\
        <mensajes>Mensaje 2 vs Mensaje 3</mensajes>\n\
        <similitud>{sim_23:.1}</similitud>\n\
        <cumpleUmbral>{cumple_23}</cumpleUmbral>\n\
      </comparacion>\n\
    </comparaciones>{rec_xml}\n\
  </data>\n\
</response>",
        xml_escape(m1),
        xml_escape(m2),
        xml_escape(m3),
    )
}

fn error_xml(msg: &str) -> Response {
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<response><status>ERROR</status><message>{}</message></response>",
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
