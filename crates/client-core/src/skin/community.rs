//! Bounded community keyboard-skin browsing independent of UI and platform hosts.
//! Source: MSIME-Apple@9ca823ab40018ced3cb71812503dbc3b94615ac0
//! (`SkinCommunityAPI.swift`, `CustomKeyboardSkin.swift`).

use crate::account::{
    AccountApi, AccountError, AccountSessionStorage, BackendAccountClient, BackendAccountSession,
};
use crate::preferences::TouchKeyboardSkinDesign;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

const MAXIMUM_OFFSET: usize = 1_000_000;
const MAXIMUM_PAGE_ITEMS: usize = 20;
const MAXIMUM_SEARCH_CHARACTERS: usize = 128;
const MAXIMUM_JAVASCRIPT_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommunitySkin {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub author: String,
    pub design: TouchKeyboardSkinDesign,
    pub downloads: u64,
    pub rating_count: u64,
    pub rating_average: f64,
    pub owned: bool,
    pub my_rating: u8,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommunitySkinPage {
    pub skins: Vec<CommunitySkin>,
    pub has_more: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommunitySkinDownload {
    design: TouchKeyboardSkinDesign,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommunitySkinRating {
    stars: u8,
}

#[derive(Serialize)]
struct CommunitySkinPublishRequest<'a> {
    id: Uuid,
    name: &'a str,
    description: &'a str,
    design: &'a TouchKeyboardSkinDesign,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommunitySkinPublishResponse {
    id: Uuid,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommunitySkinUnpublishResponse {
    deleted: bool,
}

pub trait CommunitySkinApi: Send + Sync + 'static {
    fn community_skins(
        &self,
        offset: usize,
        search: &str,
        token: Option<&str>,
    ) -> Result<CommunitySkinPage, AccountError>;
    fn community_skin(&self, id: Uuid, token: Option<&str>) -> Result<CommunitySkin, AccountError>;
    fn download_community_skin(
        &self,
        id: Uuid,
        token: &str,
    ) -> Result<TouchKeyboardSkinDesign, AccountError>;
    fn rate_community_skin(&self, id: Uuid, stars: u8, token: &str) -> Result<(), AccountError>;
    fn publish_community_skin(
        &self,
        id: Uuid,
        name: &str,
        description: &str,
        design: &TouchKeyboardSkinDesign,
        token: &str,
    ) -> Result<(), AccountError>;
    fn unpublish_community_skin(&self, id: Uuid, token: &str) -> Result<(), AccountError>;
}

impl CommunitySkinApi for BackendAccountClient {
    fn community_skins(
        &self,
        offset: usize,
        search: &str,
        token: Option<&str>,
    ) -> Result<CommunitySkinPage, AccountError> {
        validate_query(offset, search)?;
        let path = format!(
            "/v1/community/skins?offset={offset}&q={}",
            encode_query(search)
        );
        let page = self.json::<CommunitySkinPage, ()>(Method::GET, &path, token, None)?;
        validate_page(&page)?;
        Ok(page)
    }

    fn community_skin(&self, id: Uuid, token: Option<&str>) -> Result<CommunitySkin, AccountError> {
        if id.is_nil() {
            return Err(AccountError::Invalid);
        }
        let path = format!("/v1/community/skins/{}", id.hyphenated());
        let skin = self.json::<CommunitySkin, ()>(Method::GET, &path, token, None)?;
        validate_skin(&skin)?;
        if skin.id != id {
            return Err(AccountError::Unavailable);
        }
        Ok(skin)
    }

    fn download_community_skin(
        &self,
        id: Uuid,
        token: &str,
    ) -> Result<TouchKeyboardSkinDesign, AccountError> {
        if id.is_nil() {
            return Err(AccountError::Invalid);
        }
        let path = format!("/v1/community/skins/{}/download", id.hyphenated());
        let result =
            self.json::<CommunitySkinDownload, ()>(Method::POST, &path, Some(token), None)?;
        if !result.design.validate() {
            return Err(AccountError::Unavailable);
        }
        Ok(result.design.normalized())
    }

    fn rate_community_skin(&self, id: Uuid, stars: u8, token: &str) -> Result<(), AccountError> {
        if id.is_nil() || !(1..=5).contains(&stars) {
            return Err(AccountError::Invalid);
        }
        #[derive(Serialize)]
        struct RatingRequest {
            stars: u8,
        }
        let path = format!("/v1/community/skins/{}/rating", id.hyphenated());
        let result = self.json::<CommunitySkinRating, _>(
            Method::PUT,
            &path,
            Some(token),
            Some(&RatingRequest { stars }),
        )?;
        if result.stars != stars {
            return Err(AccountError::Unavailable);
        }
        Ok(())
    }

    fn publish_community_skin(
        &self,
        id: Uuid,
        name: &str,
        description: &str,
        design: &TouchKeyboardSkinDesign,
        token: &str,
    ) -> Result<(), AccountError> {
        validate_publish(id, name, description, design)?;
        let path = "/v1/community/skins";
        let result = self.json::<CommunitySkinPublishResponse, _>(
            Method::POST,
            path,
            Some(token),
            Some(&CommunitySkinPublishRequest {
                id,
                name,
                description,
                design,
            }),
        )?;
        if result.id != id {
            return Err(AccountError::Unavailable);
        }
        Ok(())
    }

    fn unpublish_community_skin(&self, id: Uuid, token: &str) -> Result<(), AccountError> {
        if id.is_nil() {
            return Err(AccountError::Invalid);
        }
        let path = format!("/v1/community/skins/{}", id.hyphenated());
        let result = self.json::<CommunitySkinUnpublishResponse, ()>(
            Method::DELETE,
            &path,
            Some(token),
            None,
        )?;
        if !result.deleted {
            return Err(AccountError::Unavailable);
        }
        Ok(())
    }
}

pub struct BackendCommunitySkinService<A: AccountApi, S: AccountSessionStorage> {
    api: A,
    session: Arc<BackendAccountSession<A, S>>,
}

impl<A: AccountApi, S: AccountSessionStorage> BackendCommunitySkinService<A, S> {
    pub fn new(api: A, session: Arc<BackendAccountSession<A, S>>) -> Self {
        Self { api, session }
    }
}

impl<A, S> BackendCommunitySkinService<A, S>
where
    A: AccountApi + CommunitySkinApi,
    S: AccountSessionStorage,
{
    pub fn list(&self, offset: usize, search: &str) -> Result<CommunitySkinPage, AccountError> {
        validate_query(offset, search)?;
        self.request(false, |api, token| {
            api.community_skins(offset, search, token)
        })
    }

    pub fn detail(&self, id: Uuid) -> Result<CommunitySkin, AccountError> {
        if id.is_nil() {
            return Err(AccountError::Invalid);
        }
        self.request(false, |api, token| api.community_skin(id, token))
    }

    pub fn download(&self, id: Uuid) -> Result<TouchKeyboardSkinDesign, AccountError> {
        if id.is_nil() {
            return Err(AccountError::Invalid);
        }
        self.request(true, |api, token| {
            api.download_community_skin(id, token.ok_or(AccountError::Unauthorized)?)
        })
    }

    pub fn rate(&self, id: Uuid, stars: u8) -> Result<(), AccountError> {
        if id.is_nil() || !(1..=5).contains(&stars) {
            return Err(AccountError::Invalid);
        }
        self.request(true, |api, token| {
            api.rate_community_skin(id, stars, token.ok_or(AccountError::Unauthorized)?)
        })
    }

    pub fn publish(
        &self,
        id: Uuid,
        name: &str,
        description: &str,
        design: &TouchKeyboardSkinDesign,
    ) -> Result<(), AccountError> {
        validate_publish(id, name, description, design)?;
        self.request(true, |api, token| {
            api.publish_community_skin(
                id,
                name,
                description,
                design,
                token.ok_or(AccountError::Unauthorized)?,
            )
        })
    }

    pub fn unpublish(&self, id: Uuid) -> Result<(), AccountError> {
        if id.is_nil() {
            return Err(AccountError::Invalid);
        }
        self.request(true, |api, token| {
            api.unpublish_community_skin(id, token.ok_or(AccountError::Unauthorized)?)
        })
    }

    fn request<T>(
        &self,
        authenticated: bool,
        operation: impl Fn(&A, Option<&str>) -> Result<T, AccountError>,
    ) -> Result<T, AccountError> {
        let identity = if self.session.status()?.is_some() {
            Some(self.session.credentials(None, None)?)
        } else {
            None
        };
        if authenticated && identity.is_none() {
            return Err(AccountError::Unauthorized);
        }
        let mut active_token = identity.as_ref().map(|value| value.1.clone());
        let result = match operation(&self.api, active_token.as_deref()) {
            Err(AccountError::Unauthorized) if identity.is_some() => {
                let expected = identity.as_ref().map(|value| value.0.as_str());
                let (_, replacement) = self
                    .session
                    .credentials(active_token.as_deref(), expected)?;
                active_token = Some(replacement);
                operation(&self.api, active_token.as_deref())
            }
            result => result,
        }?;
        let expected = identity.as_ref().map(|value| value.0.as_str());
        let current = self.session.status()?.map(|user| user.id);
        if current.as_deref() != expected {
            return Err(AccountError::Cancelled);
        }
        Ok(result)
    }
}

fn validate_query(offset: usize, search: &str) -> Result<(), AccountError> {
    if offset > MAXIMUM_OFFSET
        || search.chars().count() > MAXIMUM_SEARCH_CHARACTERS
        || search.chars().any(char::is_control)
    {
        return Err(AccountError::Invalid);
    }
    Ok(())
}

fn validate_publish(
    id: Uuid,
    name: &str,
    description: &str,
    design: &TouchKeyboardSkinDesign,
) -> Result<(), AccountError> {
    if id.is_nil()
        || !valid_text(name, 1, 32, false)
        || name.trim() != name
        || !valid_text(description, 0, 280, true)
        || !design.validate()
    {
        return Err(AccountError::Invalid);
    }
    Ok(())
}

fn validate_page(page: &CommunitySkinPage) -> Result<(), AccountError> {
    if page.skins.len() > MAXIMUM_PAGE_ITEMS
        || (page.has_more && page.skins.is_empty())
        || page.skins.iter().any(|skin| validate_skin(skin).is_err())
    {
        return Err(AccountError::Unavailable);
    }
    let mut ids = std::collections::BTreeSet::new();
    if page.skins.iter().any(|skin| !ids.insert(skin.id)) {
        return Err(AccountError::Unavailable);
    }
    Ok(())
}

fn validate_skin(skin: &CommunitySkin) -> Result<(), AccountError> {
    if !valid_text(&skin.name, 1, 32, false)
        || skin.name.trim() != skin.name
        || !valid_text(&skin.description, 0, 280, true)
        || !valid_text(&skin.author, 1, 128, false)
        || skin.author.trim() != skin.author
        || !skin.design.validate()
        || !skin.rating_average.is_finite()
        || !(0.0..=5.0).contains(&skin.rating_average)
        || skin.downloads > MAXIMUM_JAVASCRIPT_INTEGER
        || skin.rating_count > MAXIMUM_JAVASCRIPT_INTEGER
        || skin.my_rating > 5
        || (skin.rating_count == 0 && skin.rating_average != 0.0)
    {
        return Err(AccountError::Unavailable);
    }
    Ok(())
}

fn valid_text(value: &str, minimum: usize, maximum: usize, multiline: bool) -> bool {
    let count = value.chars().count();
    (minimum..=maximum).contains(&count)
        && value.chars().all(|character| {
            !character.is_control() || (multiline && matches!(character, '\n' | '\t'))
        })
}

fn encode_query(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{
        AccountChallenge, AccountProfile, AccountTokens, AccountUser, SavedAccountSession,
    };
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Mutex};

    fn token(byte: u8) -> String {
        std::iter::repeat_n(char::from(byte), 64).collect()
    }

    fn user() -> AccountUser {
        AccountUser {
            id: "fixture-user".into(),
            display_name: "Fixture".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn tokens(access: u8, refresh: u8) -> AccountTokens {
        AccountTokens {
            access_token: token(access),
            refresh_token: token(refresh),
            token_type: "Bearer".into(),
            expires_in: 900,
            user: user(),
        }
    }

    fn skin() -> CommunitySkin {
        CommunitySkin {
            id: Uuid::parse_str("10000000-0000-4000-8000-000000000001").unwrap(),
            name: "合成皮肤".into(),
            description: "仅用于协议测试".into(),
            author: "Fixture".into(),
            design: TouchKeyboardSkinDesign::default(),
            downloads: 2,
            rating_count: 1,
            rating_average: 4.0,
            owned: false,
            my_rating: 0,
        }
    }

    #[derive(Clone, Default)]
    struct MemoryStorage(Arc<Mutex<Option<SavedAccountSession>>>);

    impl AccountSessionStorage for MemoryStorage {
        fn load(&self) -> Result<Option<SavedAccountSession>, AccountError> {
            self.0
                .lock()
                .map(|value| value.clone())
                .map_err(|_| AccountError::Storage)
        }
        fn save(&self, session: &SavedAccountSession) -> Result<(), AccountError> {
            *self.0.lock().map_err(|_| AccountError::Storage)? = Some(session.clone());
            Ok(())
        }
        fn clear(&self) -> Result<(), AccountError> {
            *self.0.lock().map_err(|_| AccountError::Storage)? = None;
            Ok(())
        }
    }

    #[derive(Clone, Default)]
    struct FakeApi {
        skin_calls: Arc<AtomicUsize>,
    }

    impl AccountApi for FakeApi {
        fn providers(&self) -> Result<HashMap<String, bool>, AccountError> {
            Ok(HashMap::new())
        }
        fn challenge(&self, _: &str, _: &str) -> Result<AccountChallenge, AccountError> {
            Err(AccountError::Unavailable)
        }
        fn login(&self, _: &str, _: &str) -> Result<AccountTokens, AccountError> {
            Err(AccountError::Unavailable)
        }
        fn refresh(&self, _: &str) -> Result<AccountTokens, AccountError> {
            Ok(tokens(b'c', b'd'))
        }
        fn profile(&self, _: &str) -> Result<AccountProfile, AccountError> {
            Err(AccountError::Unavailable)
        }
        fn rename(&self, _: &str, _: &str) -> Result<(), AccountError> {
            Err(AccountError::Unavailable)
        }
        fn logout(&self, _: &str, _: bool) -> Result<(), AccountError> {
            Ok(())
        }
        fn delete_account(&self, _: &str) -> Result<(), AccountError> {
            Ok(())
        }
    }

    impl CommunitySkinApi for FakeApi {
        fn community_skins(
            &self,
            _: usize,
            _: &str,
            bearer: Option<&str>,
        ) -> Result<CommunitySkinPage, AccountError> {
            self.skin_calls.fetch_add(1, Ordering::SeqCst);
            if bearer == Some(token(b'a').as_str()) {
                return Err(AccountError::Unauthorized);
            }
            Ok(CommunitySkinPage {
                skins: vec![skin()],
                has_more: false,
            })
        }
        fn community_skin(&self, id: Uuid, _: Option<&str>) -> Result<CommunitySkin, AccountError> {
            let mut value = skin();
            value.id = id;
            Ok(value)
        }
        fn download_community_skin(
            &self,
            _: Uuid,
            bearer: &str,
        ) -> Result<TouchKeyboardSkinDesign, AccountError> {
            self.skin_calls.fetch_add(1, Ordering::SeqCst);
            if bearer == token(b'a') {
                return Err(AccountError::Unauthorized);
            }
            Ok(TouchKeyboardSkinDesign::default())
        }
        fn rate_community_skin(&self, _: Uuid, _: u8, _: &str) -> Result<(), AccountError> {
            self.skin_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn publish_community_skin(
            &self,
            _: Uuid,
            _: &str,
            _: &str,
            _: &TouchKeyboardSkinDesign,
            _: &str,
        ) -> Result<(), AccountError> {
            self.skin_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn unpublish_community_skin(&self, _: Uuid, _: &str) -> Result<(), AccountError> {
            self.skin_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn authenticated_reads_refresh_once_after_unauthorized() {
        let storage = MemoryStorage::default();
        *storage.0.lock().unwrap() = Some(SavedAccountSession {
            tokens: tokens(b'a', b'b'),
            expires_at_unix_ms: u64::MAX,
        });
        let api = FakeApi::default();
        let calls = Arc::clone(&api.skin_calls);
        let session = Arc::new(BackendAccountSession::new(api.clone(), storage));
        let service = BackendCommunitySkinService::new(api, session);
        assert_eq!(service.list(0, "").unwrap().skins.len(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn anonymous_reads_do_not_require_or_create_a_session() {
        let storage = MemoryStorage::default();
        let api = FakeApi::default();
        let session = Arc::new(BackendAccountSession::new(api.clone(), storage.clone()));
        let service = BackendCommunitySkinService::new(api, session);
        assert_eq!(service.detail(skin().id).unwrap().name, "合成皮肤");
        assert!(storage.load().unwrap().is_none());
    }

    #[test]
    fn writes_require_login_refresh_once_and_validate_stars() {
        let storage = MemoryStorage::default();
        let api = FakeApi::default();
        let session = Arc::new(BackendAccountSession::new(api.clone(), storage.clone()));
        let service = BackendCommunitySkinService::new(api.clone(), session);
        assert_eq!(service.detail(Uuid::nil()), Err(AccountError::Invalid));
        assert_eq!(service.download(Uuid::nil()), Err(AccountError::Invalid));
        assert_eq!(service.rate(Uuid::nil(), 5), Err(AccountError::Invalid));
        assert_eq!(service.unpublish(Uuid::nil()), Err(AccountError::Invalid));
        assert_eq!(service.download(skin().id), Err(AccountError::Unauthorized));
        assert_eq!(service.rate(skin().id, 0), Err(AccountError::Invalid));
        assert_eq!(api.skin_calls.load(Ordering::SeqCst), 0);

        *storage.0.lock().unwrap() = Some(SavedAccountSession {
            tokens: tokens(b'a', b'b'),
            expires_at_unix_ms: u64::MAX,
        });
        let session = Arc::new(BackendAccountSession::new(api.clone(), storage));
        let service = BackendCommunitySkinService::new(api.clone(), session);
        assert_eq!(
            service.download(skin().id).unwrap(),
            TouchKeyboardSkinDesign::default()
        );
        assert_eq!(api.skin_calls.load(Ordering::SeqCst), 2);
        service.rate(skin().id, 5).unwrap();
        assert_eq!(api.skin_calls.load(Ordering::SeqCst), 3);
        service
            .publish(
                skin().id,
                "发布皮肤",
                "公开说明",
                &TouchKeyboardSkinDesign::default(),
            )
            .unwrap();
        service.unpublish(skin().id).unwrap();
        assert_eq!(api.skin_calls.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn rejects_invalid_queries_and_skin_metadata() {
        assert_eq!(
            validate_query(MAXIMUM_OFFSET + 1, ""),
            Err(AccountError::Invalid)
        );
        assert_eq!(validate_query(0, "bad\nquery"), Err(AccountError::Invalid));
        assert_eq!(
            validate_publish(
                Uuid::nil(),
                "名称",
                "说明",
                &TouchKeyboardSkinDesign::default()
            ),
            Err(AccountError::Invalid)
        );
        assert_eq!(
            validate_publish(
                Uuid::new_v4(),
                "名称\n",
                "说明",
                &TouchKeyboardSkinDesign::default()
            ),
            Err(AccountError::Invalid)
        );
        let mut value = skin();
        value.rating_average = f64::NAN;
        assert_eq!(validate_skin(&value), Err(AccountError::Unavailable));
        value = skin();
        value.downloads = MAXIMUM_JAVASCRIPT_INTEGER + 1;
        assert_eq!(validate_skin(&value), Err(AccountError::Unavailable));
        let page = CommunitySkinPage {
            skins: vec![skin(), skin()],
            has_more: false,
        };
        assert_eq!(validate_page(&page), Err(AccountError::Unavailable));
    }

    #[test]
    fn transport_percent_encodes_search_and_validates_response() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let (sent, received) = mpsc::channel();
        let response = serde_json::to_vec(&CommunitySkinPage {
            skins: vec![skin()],
            has_more: false,
        })
        .unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let length = stream.read(&mut request).unwrap();
            sent.send(String::from_utf8_lossy(&request[..length]).into_owned())
                .unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n",
                response.len()
            )
            .unwrap();
            stream.write_all(&response).unwrap();
        });
        let client = BackendAccountClient::loopback(&origin).unwrap();
        let page = client.community_skins(7, "C++ 星", None).unwrap();
        assert_eq!(page.skins.len(), 1);
        assert!(received
            .recv()
            .unwrap()
            .starts_with("GET /v1/community/skins?offset=7&q=C%2B%2B%20%E6%98%9F HTTP/1.1"));
    }

    #[test]
    fn transport_uses_authenticated_write_contracts() {
        fn server(response: Vec<u8>) -> (String, mpsc::Receiver<String>) {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let (sent, received) = mpsc::channel();
            std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 4096];
                let length = stream.read(&mut request).unwrap();
                sent.send(String::from_utf8_lossy(&request[..length]).into_owned())
                    .unwrap();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n",
                    response.len()
                )
                .unwrap();
                stream.write_all(&response).unwrap();
            });
            (origin, received)
        }

        let id = skin().id;
        let (origin, received) = server(
            serde_json::to_vec(&serde_json::json!({
                "design": TouchKeyboardSkinDesign::default()
            }))
            .unwrap(),
        );
        let client = BackendAccountClient::loopback(&origin).unwrap();
        client.download_community_skin(id, &token(b'a')).unwrap();
        let request = received.recv().unwrap();
        assert!(request.starts_with(&format!(
            "POST /v1/community/skins/{}/download HTTP/1.1",
            id.hyphenated()
        )));
        assert!(request.contains("authorization: Bearer "));

        let (origin, received) = server(br#"{"stars":4}"#.to_vec());
        let client = BackendAccountClient::loopback(&origin).unwrap();
        client.rate_community_skin(id, 4, &token(b'b')).unwrap();
        let request = received.recv().unwrap();
        assert!(request.starts_with(&format!(
            "PUT /v1/community/skins/{}/rating HTTP/1.1",
            id.hyphenated()
        )));
        assert!(request.ends_with("\r\n\r\n{\"stars\":4}"));

        let (origin, received) =
            server(serde_json::to_vec(&serde_json::json!({ "id": id })).unwrap());
        let client = BackendAccountClient::loopback(&origin).unwrap();
        client
            .publish_community_skin(
                id,
                "发布皮肤",
                "公开说明",
                &TouchKeyboardSkinDesign::default(),
                &token(b'c'),
            )
            .unwrap();
        let request = received.recv().unwrap();
        assert!(request.starts_with("POST /v1/community/skins HTTP/1.1"));
        assert!(request.contains("authorization: Bearer "));
        let body = request.split("\r\n\r\n").nth(1).unwrap();
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(body["id"], id.to_string());
        assert_eq!(body["name"], "发布皮肤");
        assert_eq!(body["description"], "公开说明");
        assert!(body["design"].is_object());

        let (origin, received) = server(br#"{"deleted":true}"#.to_vec());
        let client = BackendAccountClient::loopback(&origin).unwrap();
        client.unpublish_community_skin(id, &token(b'd')).unwrap();
        let request = received.recv().unwrap();
        assert!(request.starts_with(&format!(
            "DELETE /v1/community/skins/{} HTTP/1.1",
            id.hyphenated()
        )));
        assert!(request.contains("authorization: Bearer "));
    }
}
