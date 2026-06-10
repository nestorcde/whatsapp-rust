use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::task;

/// Oracle connection parameters.
#[derive(Clone, Debug)]
pub struct OracleConfig {
    pub user: String,
    pub password: String,
    pub url: String,
}

impl OracleConfig {
    fn connect(&self) -> Result<oracle::Connection> {
        oracle::Connection::connect(&self.user, &self.password, &self.url)
            .context("Oracle connection failed — ensure Oracle Instant Client is installed")
    }
}

/// One row returned by the send-message / resend-message queries.
#[derive(Debug)]
pub struct MsgRow {
    pub secuencia: i32,
    pub numero: String,
    pub nombre: String,
    pub socnro: Option<f64>,
    pub msg: String,
    pub msg2: Option<String>,
    pub msg3: Option<String>,
    pub msg2_chk: i32,
    pub msg3_chk: i32,
    pub img1_chk: i32,
    pub img2_chk: i32,
    pub img3_chk: i32,
    pub img1: Option<Vec<u8>>,
    pub img2: Option<Vec<u8>>,
    pub img3: Option<Vec<u8>>,
}

// ── SQL constants (mirrors messageController.ts queries exactly) ──────────────

const SQL_PENDING: &str = "
    SELECT B.WHACRESEC AS secuencia, B.WHACRENROTEL AS numero, trim(B.WHACRENOMBRE) AS nombre,
        B.WHACRESOCNRO AS socnro,
        CASE WHEN length(trim(B.WHACREMSGIND)) > 0 THEN trim(B.WHACREMSGIND)
             ELSE CASE WHEN length(trim(A.WHACRESLD)) > 0
                  THEN concat(trim(A.WHACRESLD), CONCAT(' ', concat(trim(B.WHACRENOMBRE), CONCAT(', ', trim(A.WHACREMSG)))))
                  ELSE trim(A.WHACREMSG) END END AS msg,
        CASE WHEN length(trim(B.WHACREMSGIND)) > 0 THEN trim(B.WHACREMSGIND)
             ELSE CASE WHEN length(trim(A.WHACRESLD)) > 0
                  THEN concat(trim(A.WHACRESLD), CONCAT(' ', concat(trim(B.WHACRENOMBRE), CONCAT(', ', trim(A.WHACREMSG2)))))
                  ELSE trim(A.WHACREMSG2) END END AS msg2,
        CASE WHEN length(trim(B.WHACREMSGIND)) > 0 THEN trim(B.WHACREMSGIND)
             ELSE CASE WHEN length(trim(A.WHACRESLD)) > 0
                  THEN concat(trim(A.WHACRESLD), CONCAT(' ', concat(trim(B.WHACRENOMBRE), CONCAT(', ', trim(A.WHACREMSG3)))))
                  ELSE trim(A.WHACREMSG3) END END AS msg3,
        A.WHACREMSG2CHK AS msg2Chk, A.WHACREMSG3CHK AS msg3Chk,
        A.WHACREMSGIMGCHK AS img1Chk, A.WHACREMSG2IMGCHK AS img2Chk, A.WHACREMSG3IMGCHK AS img3Chk,
        A.WHACREMSGIMG AS img1, A.WHACREMSGIMG2 AS img2, A.WHACREMSGIMG3 AS img3
    FROM whatn001 A, whatn0011 B
    WHERE A.WHACRECOD = B.WHACRECOD
      AND B.WHACREMSGEST = 'PEN'
      AND (LENGTH(TRIM(A.WHACREMSG)) > 0 OR LENGTH(TRIM(B.WHACREMSGIND)) > 0)
      AND A.WHACRECOD = :1 AND trim(A.WHACREOFI) = :2 AND rownum <= :3";

const SQL_RESEND: &str = "
    SELECT B.WHACRESEC AS secuencia, B.WHACRENROTEL AS numero, trim(B.WHACRENOMBRE) AS nombre,
        B.WHACRESOCNRO AS socnro,
        CASE WHEN length(trim(B.WHACREMSGIND)) > 0 THEN trim(B.WHACREMSGIND)
             ELSE CASE WHEN length(trim(A.WHACRESLD)) > 0
                  THEN concat(trim(A.WHACRESLD), CONCAT(' ', concat(trim(B.WHACRENOMBRE), CONCAT(', ', trim(A.WHACREMSG)))))
                  ELSE trim(A.WHACREMSG) END END AS msg,
        CASE WHEN length(trim(B.WHACREMSGIND)) > 0 THEN trim(B.WHACREMSGIND)
             ELSE CASE WHEN length(trim(A.WHACRESLD)) > 0
                  THEN concat(trim(A.WHACRESLD), CONCAT(' ', concat(trim(B.WHACRENOMBRE), CONCAT(', ', trim(A.WHACREMSG2)))))
                  ELSE trim(A.WHACREMSG2) END END AS msg2,
        CASE WHEN length(trim(B.WHACREMSGIND)) > 0 THEN trim(B.WHACREMSGIND)
             ELSE CASE WHEN length(trim(A.WHACRESLD)) > 0
                  THEN concat(trim(A.WHACRESLD), CONCAT(' ', concat(trim(B.WHACRENOMBRE), CONCAT(', ', trim(A.WHACREMSG3)))))
                  ELSE trim(A.WHACREMSG3) END END AS msg3,
        A.WHACREMSG2CHK AS msg2Chk, A.WHACREMSG3CHK AS msg3Chk,
        A.WHACREMSGIMGCHK AS img1Chk, A.WHACREMSG2IMGCHK AS img2Chk, A.WHACREMSG3IMGCHK AS img3Chk,
        A.WHACREMSGIMG AS img1, A.WHACREMSGIMG2 AS img2, A.WHACREMSGIMG3 AS img3
    FROM whatn001 A, whatn0011 B
    WHERE A.WHACRECOD = B.WHACRECOD
      AND B.WHACREMSGEST = 'ENV'
      AND (LENGTH(TRIM(A.WHACREMSG)) > 0 OR LENGTH(TRIM(B.WHACREMSGIND)) > 0)
      AND A.WHACRECOD = :1 AND trim(A.WHACREOFI) = :2 AND rownum <= :3
      AND (SELECT COUNT(*) FROM WHATN001 w, WHATN0011 w2, WHATN002 w3
           WHERE w.WHACRECOD = w2.WHACRECOD
             AND (trim(w2.WHACRENROTEL) = TRIM(w3.WHARESNRODEST)
                  OR TRIM(w2.WHACRENROTEL) = TRIM(w3.WHARESNROORIGEN))
             AND w3.WHARESHORAFECHA >= w.WHACREFCH
             AND w.WHACRECOD = A.WHACRECOD AND TRIM(w.WHACREOFI) = trim(A.WHACREOFI)
             AND TRIM(w2.WHACRENROTEL) = TRIM(B.WHACRENROTEL)) <= :4";

// ── Helpers ──────────────────────────────────────────────────────────────────

fn optional_str(row: &oracle::Row, col: &str) -> Option<String> {
    row.get::<_, Option<String>>(col)
        .ok()
        .flatten()
        .and_then(|s| if s.trim().is_empty() { None } else { Some(s) })
}

fn collect_msg_rows(mut rs: oracle::ResultSet<oracle::Row>) -> Result<Vec<MsgRow>> {
    let mut rows = Vec::new();
    for row_result in &mut rs {
        let row = row_result?;
        rows.push(MsgRow {
            secuencia: row.get("SECUENCIA")?,
            numero: row.get("NUMERO")?,
            nombre: row.get::<_, Option<String>>("NOMBRE")?.unwrap_or_default(),
            socnro: row.get::<_, Option<f64>>("SOCNRO").ok().flatten(),
            msg: row.get::<_, Option<String>>("MSG")?.unwrap_or_default(),
            msg2: optional_str(&row, "MSG2"),
            msg3: optional_str(&row, "MSG3"),
            msg2_chk: row.get::<_, Option<i32>>("MSG2CHK")?.unwrap_or(0),
            msg3_chk: row.get::<_, Option<i32>>("MSG3CHK")?.unwrap_or(0),
            img1_chk: row.get::<_, Option<i32>>("IMG1CHK")?.unwrap_or(0),
            img2_chk: row.get::<_, Option<i32>>("IMG2CHK")?.unwrap_or(0),
            img3_chk: row.get::<_, Option<i32>>("IMG3CHK")?.unwrap_or(0),
            img1: row.get("IMG1").ok().flatten(),
            img2: row.get("IMG2").ok().flatten(),
            img3: row.get("IMG3").ok().flatten(),
        });
    }
    Ok(rows)
}

// ── Public async API ─────────────────────────────────────────────────────────

/// Upsert bearer token for a session in WHATN003.
/// Mirrors `encryptController.ts` → `buscaOficial`/`updateOficial`/`insertOficial`.
pub async fn upsert_oficial_token(cfg: Arc<OracleConfig>, session: String, token: String) -> Result<()> {
    task::spawn_blocking(move || {
        let conn = cfg.connect()?;

        let count: i64 = {
            let mut rs = conn.query(
                "SELECT COUNT(*) FROM WHATN003 WHERE trim(WHAOFICOD) = :1",
                &[&session],
            )?;
            rs.next()
                .context("COUNT(*) returned no rows")?
                .context("COUNT(*) row error")?
                .get(0)?
        };

        if count > 0 {
            conn.execute(
                "UPDATE WHATN003 SET WHAOFITOKEN=:1 WHERE trim(WHAOFICOD)=:2",
                &[&token, &session],
            )?;
        } else {
            conn.execute(
                "INSERT INTO WHATN003 (WHAOFICOD, WHAOFITOKEN) VALUES (:1, :2)",
                &[&session, &token],
            )?;
        }
        conn.commit().context("commit failed")
    })
    .await?
}

/// Store QR code bytes (as hex) into WHATN001.WHACREQRCOD (Oracle RAW column).
/// `qr_hex` is a lowercase hex string of the QR PNG bytes.
pub async fn update_qr_code(cfg: Arc<OracleConfig>, whacrecod: String, qr_hex: String) -> Result<()> {
    if qr_hex.is_empty() {
        return Ok(());
    }
    task::spawn_blocking(move || {
        let conn = cfg.connect()?;
        conn.execute(
            "UPDATE WHATN001 SET WHACREQRCOD=HEXTORAW(:1) WHERE WHACRECOD=:2",
            &[&qr_hex, &whacrecod],
        )?;
        conn.commit().context("commit failed")
    })
    .await?
}

/// Fetch pending (`WHACREMSGEST = 'PEN'`) messages for send-message-gx.
pub async fn get_pending_messages(
    cfg: Arc<OracleConfig>,
    whacrecod: String,
    whacreofi: String,
    cantidad: i32,
) -> Result<Vec<MsgRow>> {
    task::spawn_blocking(move || {
        let conn = cfg.connect()?;
        let rs = conn.query(SQL_PENDING, &[&whacrecod, &whacreofi, &cantidad])?;
        collect_msg_rows(rs)
    })
    .await?
}

/// Fetch already-sent (`WHACREMSGEST = 'ENV'`) messages for resend-message-gx.
pub async fn get_resend_messages(
    cfg: Arc<OracleConfig>,
    whacrecod: String,
    whacreofi: String,
    cantidad: i32,
    veces: i32,
) -> Result<Vec<MsgRow>> {
    task::spawn_blocking(move || {
        let conn = cfg.connect()?;
        let rs = conn.query(SQL_RESEND, &[&whacrecod, &whacreofi, &cantidad, &veces])?;
        collect_msg_rows(rs)
    })
    .await?
}

/// Data for a single WHATN002 history row (INI/DES/MSG).
pub struct Whatn002Row {
    pub ofi: String,
    pub nro_origen: String,
    pub nro_dest: String,
    pub mensaje: String,
    pub fecha_hora: String,
    pub propio: i32,
    pub tipo: String,
    pub has_media: i32,
    pub mimetype: String,
    pub mediadata: String,
}

/// Insert a row into WHATN002 (session/message history).
pub async fn insert_whatn002(cfg: Arc<OracleConfig>, row: Whatn002Row) -> Result<()> {
    task::spawn_blocking(move || {
        let conn = cfg.connect()?;
        conn.execute(
            "INSERT INTO WHATN002 \
             (WHARESOFI, WHARESNROORIGEN, WHARESNRODEST, WHARESMENSAJE, WHARESHORAFECHA, \
              WHARESPROPIO, WHARESTIPO, WHARESHASMEDIA, WHARESMIMETYPE, WHARESMEDIADATA) \
             VALUES (:1,:2,:3,:4,TO_DATE(:5,'YYYY-MM-DD HH24:MI:SS'),:6,:7,:8,:9,:10)",
            &[
                &row.ofi,
                &row.nro_origen,
                &row.nro_dest,
                &row.mensaje,
                &row.fecha_hora,
                &row.propio,
                &row.tipo,
                &row.has_media,
                &row.mimetype,
                &row.mediadata,
            ],
        )?;
        conn.commit().context("commit failed")
    })
    .await?
}

/// Update WHAOFIESTADO in WHATN003 ('CONECTADO' on connect, 'DESCONECTADO' on logout).
pub async fn update_oficial_status(cfg: Arc<OracleConfig>, session: String, estado: String) -> Result<()> {
    task::spawn_blocking(move || {
        let conn = cfg.connect()?;
        conn.execute(
            "UPDATE WHATN003 SET WHAOFIESTADO=:1 WHERE trim(WHAOFICOD)=:2",
            &[&estado, &session],
        )?;
        conn.commit().context("commit failed")
    })
    .await?
}

/// After a MSG insert, increment WHACRECNTINT on the most-recent ENV row
/// in WHATN0011 whose phone matches `numero_cliente` and office matches `ofi`.
pub async fn increment_whacrecntint(
    cfg: Arc<OracleConfig>,
    ofi: String,
    numero_cliente: String,
) -> Result<()> {
    task::spawn_blocking(move || {
        let conn = cfg.connect()?;
        conn.execute(
            "UPDATE WHATN0011 SET WHACRECNTINT = WHACRECNTINT + 1 \
             WHERE ROWID = ( \
               SELECT B.ROWID FROM WHATN001 A, WHATN0011 B \
               WHERE A.WHACRECOD = B.WHACRECOD \
                 AND TRIM(A.WHACREOFI) = :1 \
                 AND (TRIM(B.WHACRENROTEL) = :2 \
                      OR '+' || TRIM(B.WHACRENROTEL) = :2) \
                 AND B.WHACREMSGEST = 'ENV' \
               ORDER BY B.WHACRECOD DESC \
               FETCH FIRST 1 ROWS ONLY \
             )",
            &[&ofi, &numero_cliente],
        )?;
        conn.commit().context("commit failed")
    })
    .await?
}

/// Update message delivery status (`"ENV"` = sent, `"FAL"` = failed) in WHATN0011.
pub async fn update_msg_status(
    cfg: Arc<OracleConfig>,
    whacrecod: String,
    secuencia: i32,
    estado: String,
) -> Result<()> {
    task::spawn_blocking(move || {
        let conn = cfg.connect()?;
        conn.execute(
            "UPDATE WHATN0011 SET WHACREMSGEST=:1, WHACREFCHHRAEST=sysdate
             WHERE WHACRECOD=:2 AND WHACRESEC=:3",
            &[&estado, &whacrecod, &secuencia],
        )?;
        conn.commit().context("commit failed")
    })
    .await?
}
