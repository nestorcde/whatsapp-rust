# Plan de Implementación: HTTP Server sobre whatsapp-rust

## Objetivo

Reemplazar el servidor wppconnect (Node.js/TypeScript + Chromium) por un servidor HTTP nativo en Rust que use `whatsapp-rust` como backend de protocolo. El nuevo servidor expone exactamente los mismos endpoints que consume Genexus hoy, sin cambiar ningún cliente existente.

### Qué cambia vs el servidor actual

| Aspecto | wppconnect (actual) | whatsapp-rust (nuevo) |
| ------- | ------------------- | --------------------- |
| Protocolo WA | Browser Chromium + Puppeteer | Protocolo nativo (Noise + Signal) |
| Runtime | Node.js 18 | Rust + Tokio |
| Persistencia sesiones | Archivos JSON + memoria | SQLite (Diesel) |
| Persistencia negocio | Oracle DB (oracledb) | Oracle DB (crate `oracle`) |
| Framework HTTP | Express.js | Axum |

---

## Endpoints a implementar

Los endpoints marcados son los que usa Genexus actualmente. Los demás se omiten en esta implementación.

### Sesiones (sin sesión WA activa requerida)

```text
POST   /api/:session/:secretkey/generate-token
GET    /api/:secretkey/show-all-sessions
```

### Sesiones (requieren auth)

```text
POST   /api/:session/start-session
GET    /api/:session/status-session
GET    /api/:session/:token/qrcode-session
```

### Mensajería GX (requieren auth + sesión conectada)

```text
POST   /api/:session/send-message-gx
POST   /api/:session/resend-message-gx
```

### Similitud léxica (sin auth, sin sesión)

```text
POST   /api/validate-lexical-similarity
```

### Google Contacts API (auth propia: `GOOGLE_CONTACTS_SECRET_KEY`)

```text
POST   /api/google-contacts/credentials/:userKey
GET    /api/google-contacts/credentials/:userKey/status
DELETE /api/google-contacts/credentials/:userKey

POST   /api/google-contacts/auth/start/:userKey
POST   /api/google-contacts/auth/callback/:userKey
GET    /api/google-contacts/auth/status/:userKey
DELETE /api/google-contacts/auth/revoke/:userKey

GET    /api/google-contacts/users
POST   /api/google-contacts/:userKey/search
POST   /api/google-contacts/:userKey/upsert
```

---

## Arquitectura del nuevo crate

Se crea un nuevo crate `server/` que se agrega al workspace existente.

```text
server/
├── Cargo.toml
└── src/
    ├── main.rs                  ← startup, bind puerto, inicializar SessionManager
    ├── config.rs                ← env vars: SECRET_KEY, DB_USER/PASS/URL, OPENAI_API_KEY, PORT
    ├── oracle.rs                ← pool de conexiones + helpers (spawn_blocking)
    ├── session_manager.rs       ← HashMap<String, SessionEntry> con Arc<RwLock>
    ├── middleware/
    │   ├── mod.rs
    │   ├── auth.rs              ← verificar Bearer token contra WHATN003 (bcrypt)
    │   └── google_auth.rs       ← verificar GOOGLE_CONTACTS_SECRET_KEY header
    └── routes/
        ├── mod.rs               ← router Axum con todas las rutas
        ├── session.rs           ← 5 endpoints de sesión
        ├── messages_gx.rs       ← send-message-gx + resend-message-gx
        ├── similarity.rs        ← validate-lexical-similarity
        └── google_contacts.rs   ← 10 endpoints Google Contacts
```

### Agregar al workspace

En `Cargo.toml` raíz, agregar `"server"` al array `members`.

---

## Variables de entorno requeridas

```env
# Servidor
PORT=21465
SECRET_KEY=THISISMYSECURETOKEN

# Oracle DB
DB_USER=...
DB_PASS=...
DB_URL=...         # connection string, ej: //host:1521/service

# OpenAI (para validate-lexical-similarity)
OPENAI_API_KEY=...

# Google Contacts
GOOGLE_CONTACTS_SECRET_KEY=...
GOOGLE_CONTACTS_TOKEN_DIR=./google-tokens  # directorio para tokens OAuth2
```

---

## Tablas Oracle utilizadas

| Tabla | Uso |
| ----- | --- |
| `WHATN001` | Campañas: mensaje, imagen (BLOB), estado, saludo, oficina |
| `WHATN0011` | Destinatarios: teléfono, nombre, estado mensaje, msg individual |
| `WHATN002` | Respuestas recibidas (para filtro de `veces` en resend) |
| `WHATN003` | Tokens por sesión: `WHAOFICOD` → `WHAOFITOKEN` |

---

## Fases de implementación

### Fase 1 — Scaffold del servidor

**Estado:** [ ] Pendiente

**Objetivo:** servidor Axum arrancando, ruta `/healthz`, compilando dentro del workspace.

**Tareas:**

- [ ] Crear `server/Cargo.toml` con dependencias base
- [ ] Agregar `"server"` a workspace members en `Cargo.toml` raíz
- [ ] `src/main.rs`: inicializar Tokio, levantar Axum en `PORT`
- [ ] `src/config.rs`: leer env vars con `std::env` o `dotenvy`
- [ ] Ruta `GET /healthz` → 200 OK
- [ ] Verificar `cargo build -p server` compila sin errores

**Dependencias en `server/Cargo.toml`:**

```toml
[dependencies]
axum            = { version = "0.8", features = ["multipart"] }
tokio           = { version = "1", features = ["full"] }
tower-http      = { version = "0.6", features = ["cors", "trace"] }
serde           = { version = "1", features = ["derive"] }
serde_json      = "1"
dotenvy         = "0.15"
tracing         = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
whatsapp-rust   = { path = "../" }
```

---

### Fase 2 — Session Manager

**Estado:** [ ] Pendiente  
**Requiere:** Fase 1 completa

**Objetivo:** gestionar el ciclo de vida de múltiples instancias de `Client` (una por session name).

**Estructura de datos:**

```rust
pub enum SessionStatus {
    Closed,
    Starting,
    WaitingQr,
    Connected,
}

pub struct SessionEntry {
    pub client: Arc<Client>,
    pub status: Arc<RwLock<SessionStatus>>,
    pub qr_string: Arc<RwLock<Option<String>>>,  // última QR string disponible
}

pub struct SessionManager {
    sessions: Arc<RwLock<HashMap<String, SessionEntry>>>,
    // factory para crear Client (reutiliza tokio-transport + sqlite-storage)
}
```

**Tareas:**

- [ ] `src/session_manager.rs`: implementar `SessionManager`
- [ ] Método `get_or_create(session_name) -> SessionEntry`
- [ ] Método `get(session_name) -> Option<SessionEntry>`
- [ ] Método `list_all() -> Vec<String>`
- [ ] Engancharse a eventos del `Client` para actualizar `status` y `qr_string`:
  - Al recibir QR → `status = WaitingQr`, `qr_string = Some(...)`
  - Al conectar → `status = Connected`, `qr_string = None`
  - Al desconectar → `status = Closed`
- [ ] Revisar cómo el `Client` de whatsapp-rust expone eventos/callbacks (ver `src/handlers/` y la API pública en `src/lib.rs`) y elegir el mecanismo correcto (canal Tokio, closure de registro, etc.)
- [ ] Inyectar `SessionManager` como estado de Axum (`axum::extract::State`)

**Nota:** cada sesión almacena su SQLite en un path único derivado del session name, por ejemplo `./sessions/{session_name}.db`. Esto replica el comportamiento del `userDataDir` de wppconnect.

---

### Fase 3 — Oracle DB integration

**Estado:** [ ] Pendiente  
**Requiere:** Fase 1 completa (puede desarrollarse en paralelo con Fase 2)

**Objetivo:** capa de acceso a Oracle para todas las queries del negocio.

**Tareas:**

- [ ] Agregar `oracle = "0.6"` a `server/Cargo.toml`
- [ ] Verificar que Oracle Instant Client está disponible en el servidor de deploy
- [ ] `src/oracle.rs`: struct `OraclePool` con `oracle::Connection` bajo `Arc<Mutex<...>>`  
  *(el crate `oracle` es síncrono — toda query usa `tokio::task::spawn_blocking`)*
- [ ] Implementar helper `get_connection() -> Result<oracle::Connection>`
- [ ] Implementar las siguientes funciones:

```rust
// Para generate-token
pub async fn upsert_oficial_token(session: &str, token: &str) -> Result<()>

// Para status-session (escribe QR code en Oracle)
pub async fn update_qr_code(whacrecod: &str, qr_hex: &str) -> Result<()>

// Para send-message-gx
pub async fn get_pending_messages(
    whacrecod: &str, whacreofi: &str, cantidad: i32
) -> Result<Vec<MsgRow>>

// Para resend-message-gx  
pub async fn get_resend_messages(
    whacrecod: &str, whacreofi: &str, cantidad: i32, veces: i32
) -> Result<Vec<MsgRow>>

// Para ambos GX endpoints
pub async fn update_msg_status(
    whacrecod: &str, secuencia: i32, estado: &str  // "ENV" | "FAL"
) -> Result<()>
```

**Tipo `MsgRow`:**

```rust
pub struct MsgRow {
    pub secuencia: i32,
    pub numero: String,
    pub msg: String,
    pub msg2: Option<String>,
    pub msg3: Option<String>,
    pub msg2_chk: i32,
    pub msg3_chk: i32,
    pub img1_chk: i32,
    pub img2_chk: i32,
    pub img3_chk: i32,
    pub img1: Option<Vec<u8>>,   // BLOB
    pub img2: Option<Vec<u8>>,
    pub img3: Option<Vec<u8>>,
}
```

---

### Fase 4 — Auth middleware

**Estado:** [ ] Pendiente  
**Requiere:** Fase 3 completa

**Objetivo:** verificar tokens Bearer en los endpoints que lo requieren.

**Lógica del middleware (idéntica al TS):**

1. Extraer header `Authorization: Bearer <token>`
2. Extraer `session` del path param
3. Buscar `WHAOFITOKEN` en `WHATN003` donde `WHAOFICOD = session`
4. Verificar con `bcrypt::verify(session + SECRET_KEY, stored_hash)`
5. Si falla → 401

**Tareas:**

- [ ] `src/middleware/auth.rs`: implementar como extractor Axum  
  *(puede usar `axum::extract::FromRequestParts` o middleware de tower)*
- [ ] `src/middleware/google_auth.rs`: verificar que el header `Authorization` sea igual a `GOOGLE_CONTACTS_SECRET_KEY` (comparación directa, sin bcrypt)
- [ ] Agregar dependencias: `bcrypt = "0.15"`, `axum-extra` si hace falta

---

### Fase 5 — Endpoints de sesión

**Estado:** [ ] Pendiente  
**Requiere:** Fases 2, 3, 4 completas

#### `POST /api/:session/:secretkey/generate-token`

- Sin auth Bearer (usa `:secretkey` en path)
- Verifica que `:secretkey == SECRET_KEY`
- Calcula `bcrypt(session + SECRET_KEY)`
- Sanitiza el hash (reemplaza `/` → `_`, `+` → `-`)
- Llama `oracle::upsert_oficial_token(session, hash)`
- Retorna `Content-Type: text/plain` + JSON: `{ status, session, token, full }`

#### `GET /api/:secretkey/show-all-sessions`

- Sin auth Bearer
- Verifica `:secretkey == SECRET_KEY`
- Retorna `{ response: [lista de session names] }` desde `SessionManager::list_all()`

#### `POST /api/:session/start-session`

- Con auth Bearer
- Si la sesión ya existe y está `Connected` → retorna estado actual
- Si no existe o está `Closed` → crea nuevo `Client`, inicia conexión, registra en `SessionManager`
- Retorna XML con estado actual (igual que `status-session`)
- Body opcional: `{ webhook, waitQrCode, proxy }` — implementar `webhook` si se necesita más adelante

#### `GET /api/:session/status-session`

- Con auth Bearer
- Retorna `Content-Type: text/plain` (responde XML)
- Lee estado y `qr_string` de `SessionEntry`
- Si hay QR disponible: llama `oracle::update_qr_code(whacrecod, qr_hex)` donde `whacrecod` viene del header `whacrecod`
- XML de respuesta: `<status>`, `<urlcode>`, `<version>`
- Si `status == Closed` → JSON `{ status: "CLOSED", qrcode: null }`

#### `GET /api/:session/:token/qrcode-session`

- Con auth Bearer
- Lee `qr_string` de `SessionEntry`
- Si existe: genera PNG usando el crate `qrcode` + `image`, retorna bytes como `image/png`
- Si no existe: retorna JSON con estado actual

**Tareas:**

- [ ] `src/routes/session.rs` con los 5 handlers
- [ ] Registrar rutas en `src/routes/mod.rs`
- [ ] Agregar `qrcode = "0.14"` y `image = "0.25"` a `server/Cargo.toml`
- [ ] Definir struct de respuesta XML (usar `quick-xml` o `xml-rs`)
- [ ] Integrar con `SessionManager` inyectado como estado

---

### Fase 6 — send-message-gx y resend-message-gx

**Estado:** [ ] Pendiente  
**Requiere:** Fase 5 completa

Estos son los endpoints más críticos del negocio. La implementación replica exactamente la lógica del TypeScript.

#### Comportamiento común a ambos

1. Query Oracle para obtener filas (ver queries en Fase 3)

2. Para cada fila:
   a. Limpiar y validar número de teléfono (formato E.164 + `@c.us`)
   b. Determinar qué mensaje enviar según turno (`msg`, `msg2`, `msg3`) y flags `msg2Chk`, `msg3Chk`
   c. Si hay imagen (`img1Chk`, `img2Chk`, `img3Chk`): guardar BLOB a disco temporal, enviar con `client.send_document()`, borrar archivo
   d. Si no hay imagen: `client.send_message(phonenumber, texto)`
   e. Actualizar Oracle: `ENV` si exitoso, `FAL` si error
   f. Retry hasta 2 veces en errores transitorios (chat no encontrado)
   g. Delay aleatorio entre `segRetDsd` y `segRetHst` segundos entre mensajes
3. **Respuesta HTTP inmediata** (text/plain): se envía "Los mensajes se enviarán..." al cliente y el envío continúa en background (Tokio task)

#### Diferencia entre send y resend

| Campo | send-message-gx | resend-message-gx |
| ----- | --------------- | ----------------- |
| Filtro estado | `WHACREMSGEST = 'PEN'` | `WHACREMSGEST = 'ENV'` |
| Parámetro extra | — | `veces`: omite números que ya recibieron ≥ N respuestas en WHATN002 |
| Body params | `whacrecod, sender, cantidad, segRetDsd, segRetHst` | Mismos + `veces` |

#### Validación de número de teléfono

Portar la función `formatAndValidateWhatsAppNumber` del TS:
1. Limpiar número (trim, quitar `@...`)
2. Validar con crate `phonenumber`
3. Formatear a E.164 sin `+`
4. Agregar `@c.us`
5. *(En TS también llama `getPnLidEntry` — evaluar si la API de whatsapp-rust expone esto)*

**Tareas:**

- [ ] `src/routes/messages_gx.rs` con los 2 handlers
- [ ] Función `format_and_validate_number(raw: &str, client: &Client) -> Result<String>`
- [ ] Función `send_message_with_retry(client, phonenumber, con_imagen, ..., max_retries) -> Result<()>`
- [ ] Lógica de turno de mensajes (`turno` 0→1→2→0 según flags `msg2Chk`, `msg3Chk`)
- [ ] Manejo de BLOBs: guardar a disco, enviar, borrar
- [ ] Agregar `phonenumber = "0.3"` y `rand = "0.9"` a `server/Cargo.toml`
- [ ] Registrar rutas en `src/routes/mod.rs`

---

### Fase 7 — validate-lexical-similarity

**Estado:** [ ] Pendiente  
**Requiere:** Fase 1 completa (independiente de WA)

Endpoint de solo cómputo. Sin auth, sin sesión WhatsApp.

#### Lógica

1. Recibir `mensaje1`, `mensaje2`, `mensaje3`, `umbralSimilitud` (0-100)
2. Si faltan mensajes (menos de 3): generar los faltantes con OpenAI
3. Calcular similitud léxica entre cada par usando Levenshtein/Jaccard normalizado
4. Evaluar si cada par cumple `similitud <= umbralSimilitud`
5. Si algún par falla Y `umbralSimilitud >= 30`: llamar OpenAI para reescribir con mayor diversidad léxica y validar las recomendaciones también
6. Retornar XML con resultados completos

#### Formato de respuesta

`Content-Type: text/xml; charset=utf-8`

```xml
<response>
  <status>SUCCESS</status>
  <message>...</message>
  <data>
    <cumpleValidacion>true</cumpleValidacion>
    <umbralConfigurado>70</umbralConfigurado>
    <comparaciones>
      <comparacion>
        <mensajes>Mensaje 1 vs Mensaje 2</mensajes>
        <similitud>45.2</similitud>
        <cumpleUmbral>true</cumpleUmbral>
      </comparacion>
      ...
    </comparaciones>
  </data>
  <!-- si no cumple, incluir recomendaciones -->
</response>
```

**Tareas:**

- [ ] `src/routes/similarity.rs` con el handler
- [ ] Función `calcular_similitud_lexica(m1, m2, m3) -> SimilitudResult` (usando `strsim`)
- [ ] Función `generar_mensajes_faltantes(m1, m2, m3, umbral) -> (String, String, String)` (OpenAI)
- [ ] Función `reescribir_mensajes(m1, m2, m3, umbral) -> Recomendaciones` (OpenAI)
- [ ] Serialización XML de respuesta (usando `quick-xml`)
- [ ] Agregar `strsim = "0.11"` y `async-openai = "0.27"` a `server/Cargo.toml`
- [ ] Registrar ruta en `src/routes/mod.rs`

---

### Fase 8 — Google Contacts API

**Estado:** [ ] Pendiente  
**Requiere:** Fase 1 completa (independiente de WA y de Oracle)

Subsistema completamente independiente. Auth propia via header con `GOOGLE_CONTACTS_SECRET_KEY`.

#### Servicios internos a implementar

```text
src/google/
├── mod.rs
├── auth_service.rs      ← OAuth2 flow, guardar/cargar tokens por userKey
├── people_service.rs    ← llamadas a Google People API (listar, crear, buscar)
└── phone_normalizer.rs  ← normalización E.164 (crate phonenumber)
```

#### Almacenamiento

Los tokens OAuth2 y credentials se guardan en archivos JSON locales bajo `GOOGLE_CONTACTS_TOKEN_DIR/{userKey}/`:

- `credentials.json` — credenciales de Google Cloud (OAuth2 client_id, client_secret)
- `token.json` — access token + refresh token del usuario

El mapeo `userKey → email` se configura en `config.rs` a partir de env vars (o un archivo de config separado).

#### Flujo OAuth2

1. `POST /auth/start/:userKey` → generar URL de autorización Google
2. Usuario abre URL, autoriza → Google redirige con `?code=...`
3. `POST /auth/callback/:userKey` con `{ code }` → intercambiar por tokens, guardar en disco
4. Requests subsiguientes usan el refresh token automáticamente

#### Endpoints de contactos

- `search`: buscar si un teléfono ya existe en Google Contacts (normalizar E.164 antes de buscar)
- `upsert`: crear contactos nuevos con deduplicación — carga todos los teléfonos existentes en memoria (prefetch) antes de procesar el batch

**Tareas:**

- [ ] `src/google/auth_service.rs`: load/save credentials y tokens por userKey
- [ ] `src/google/people_service.rs`: `list_all_contacts()`, `phone_exists()`, `create_contact()`
- [ ] `src/google/phone_normalizer.rs`: `normalize(phone) -> String`, `is_valid(phone) -> bool`
- [ ] `src/routes/google_contacts.rs`: 10 handlers
- [ ] Agregar `yup-oauth2 = "11"` y `google-people1 = "6"` (o llamadas directas con `reqwest`) a `server/Cargo.toml`
- [ ] Middleware `src/middleware/google_auth.rs` (verifica header contra `GOOGLE_CONTACTS_SECRET_KEY`)
- [ ] Registrar rutas en `src/routes/mod.rs`

---

## Checklist de progreso general

### Checklist Fase 1 — Scaffold ✅

- [x] `server/Cargo.toml` creado
- [x] Agregado al workspace
- [x] `main.rs` levanta servidor Axum en `PORT`
- [x] `/healthz` responde 200, `/api/ping` responde "pong"
- [x] `cargo build -p whatsapp-rust-server` exitoso (1 warning inofensivo de dead_code esperado)

### Checklist Fase 2 — Session Manager

- [ ] `SessionEntry` definido con status + qr_string
- [ ] `SessionManager` implementado
- [ ] Mecanismo de eventos del Client identificado e integrado
- [ ] Inyectado como estado Axum

### Checklist Fase 3 — Oracle

- [ ] Oracle Instant Client disponible en servidor
- [ ] `get_connection()` funcionando
- [ ] `upsert_oficial_token()` implementado y testeado
- [ ] `update_qr_code()` implementado
- [ ] `get_pending_messages()` implementado
- [ ] `get_resend_messages()` implementado
- [ ] `update_msg_status()` implementado

### Checklist Fase 4 — Auth

- [ ] Middleware Bearer token implementado
- [ ] Middleware Google Contacts implementado
- [ ] Verificación bcrypt funcionando

### Checklist Fase 5 — Endpoints de sesión

- [ ] `generate-token` funcionando (con Oracle)
- [ ] `show-all-sessions` funcionando
- [ ] `start-session` inicia Client y registra en SessionManager
- [ ] `status-session` retorna XML correcto + escribe QR en Oracle
- [ ] `qrcode-session` retorna PNG del QR

### Checklist Fase 6 — GX Messages

- [ ] `format_and_validate_number()` funcionando
- [ ] `send_message_with_retry()` implementado
- [ ] `send-message-gx` consulta Oracle, envía, actualiza estado
- [ ] `resend-message-gx` ídem con filtro de veces
- [ ] Manejo de imágenes BLOB funcionando
- [ ] Lógica de turnos (msg/msg2/msg3) correcta
- [ ] Delays aleatorios entre mensajes

### Checklist Fase 7 — Lexical Similarity

- [ ] `calcular_similitud_lexica()` implementado
- [ ] Generación de mensajes faltantes con OpenAI
- [ ] Reescritura con OpenAI cuando no cumple umbral
- [ ] Respuesta XML correcta

### Checklist Fase 8 — Google Contacts

- [ ] `auth_service.rs` con OAuth2 flow completo
- [ ] `people_service.rs` con prefetch y deduplicación
- [ ] `phone_normalizer.rs` funcionando
- [ ] Los 10 endpoints responden correctamente

---

## Decisiones de diseño a confirmar antes de empezar

1. **Path de SQLite por sesión:** ¿`./sessions/{name}.db` o configurable por env var?
2. **Mecanismo de eventos del Client:** verificar qué API expone `whatsapp-rust` para subscribirse a eventos (QR, conectado, desconectado) antes de diseñar `SessionManager`.
3. **`getPnLidEntry` en validación de número:** la función TS llama `client.getPnLidEntry(phone)` para resolver LID vs número de teléfono. Verificar si `whatsapp-rust` expone una API equivalente (`src/features/` → contactos).
4. **Webhook:** el `start-session` acepta un `webhook` URL. ¿Se implementa en esta fase o se deja para después?
5. **Oracle connection pool vs conexiones individuales:** el crate `oracle` no tiene pool nativo; evaluar si se necesita `r2d2-oracle` o si conexiones por-request son aceptables dado el volumen.

---

## Referencias

- Código fuente original: `wppconnect-sesrver-prod.tgz` (rama `mi-custom`)
- Rutas: `src/routes/index.ts`
- Sesiones: `src/controller/sessionController.ts`
- Mensajería GX: `src/controller/messageController.ts` (funciones `sendMessageGx`, `resendMessageGx`)
- Google Contacts: `src/controller/googleContactsController.ts`
- Token generation: `src/controller/encryptController.ts`
- Arquitectura Rust: `agent_docs/feature_implementation.md`
