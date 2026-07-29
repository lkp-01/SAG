//! 中交 4A OAuth2 授权码模式（与 `docs/中交4A认证协议说明.md` 对齐）。
use reqwest::Client;
use serde_json::Value;

#[derive(Clone)]
pub struct FourAConfig {
    pub first_uri: String,
    pub second_uri: String,
    pub third_uri: String,
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Clone)]
pub struct OidcConfig {
    pub issuer: String,
    pub authorize_uri: String,
    pub token_uri: String,
    pub userinfo_uri: String,
    pub client_id: String,
    pub client_secret: String,
    pub scopes: String,
}

pub fn config_from_env() -> Option<FourAConfig> {
    let first = std::env::var("SAG_FOURA_FIRST_URI").ok()?;
    let second = std::env::var("SAG_FOURA_SECOND_URI").ok()?;
    let third = std::env::var("SAG_FOURA_THIRD_URI").ok()?;
    let client_id = std::env::var("SAG_FOURA_CLIENT_ID").ok()?;
    let client_secret = std::env::var("SAG_FOURA_CLIENT_SECRET").ok()?;
    if first.is_empty()
        || second.is_empty()
        || third.is_empty()
        || client_id.is_empty()
        || client_secret.is_empty()
    {
        return None;
    }
    Some(FourAConfig {
        first_uri: first,
        second_uri: second,
        third_uri: third,
        client_id,
        client_secret,
    })
}

pub fn oidc_config_from_env() -> Option<OidcConfig> {
    let issuer = std::env::var("SAG_OIDC_ISSUER").ok()?;
    let token_uri = std::env::var("SAG_OIDC_TOKEN_URI").ok()?;
    let userinfo_uri = std::env::var("SAG_OIDC_USERINFO_URI").ok()?;
    let client_id = std::env::var("SAG_OIDC_CLIENT_ID").ok()?;
    let client_secret = std::env::var("SAG_OIDC_CLIENT_SECRET").ok()?;
    let authorize_uri = std::env::var("SAG_OIDC_AUTHORIZE_URI")
        .ok()
        .unwrap_or_else(|| format!("{}/authorize", issuer.trim_end_matches('/')));
    let scopes = std::env::var("SAG_OIDC_SCOPES")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "openid profile email groups".to_string());
    if issuer.is_empty()
        || token_uri.is_empty()
        || userinfo_uri.is_empty()
        || client_id.is_empty()
        || client_secret.is_empty()
    {
        return None;
    }
    Some(OidcConfig {
        issuer,
        authorize_uri,
        token_uri,
        userinfo_uri,
        client_id,
        client_secret,
        scopes,
    })
}

pub fn redirect_uri() -> String {
    std::env::var("SAG_FOURA_REDIRECT_URI").unwrap_or_else(|_| {
        let base =
            std::env::var("SAG_PUBLIC_BASE_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".into());
        format!("{}/api/v1/auth/sso/callback", base.trim_end_matches('/'))
    })
}

/// ④⑤ POST `secondUri`，`application/x-www-form-urlencoded`
pub async fn exchange_code_for_token(
    client: &Client,
    cfg: &FourAConfig,
    code: &str,
) -> Result<String, String> {
    let body = [
        ("client_id", cfg.client_id.as_str()),
        ("client_secret", cfg.client_secret.as_str()),
        ("code", code),
        ("grant_type", "authorization_code"),
    ];
    let resp = client
        .post(&cfg.second_uri)
        .form(&body)
        .send()
        .await
        .map_err(|e| format!("4A token http: {e}"))?;

    let v = resp
        .json::<Value>()
        .await
        .map_err(|e| format!("4A token json: {e}"))?;

    if let Some(ec) = v.get("errcode") {
        let bad = match ec {
            Value::String(s) => !s.is_empty() && s != "0",
            Value::Number(n) => n.as_i64() != Some(0),
            _ => false,
        };
        if bad {
            let msg = v
                .get("msg")
                .and_then(|x| x.as_str())
                .unwrap_or("4A token error");
            return Err(msg.to_string());
        }
    }

    v.get("access_token")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "missing access_token".to_string())
}

pub async fn exchange_code_for_token_oidc(
    client: &Client,
    cfg: &OidcConfig,
    code: &str,
    redirect_uri: &str,
) -> Result<Value, String> {
    let body = [
        ("client_id", cfg.client_id.as_str()),
        ("client_secret", cfg.client_secret.as_str()),
        ("code", code),
        ("grant_type", "authorization_code"),
        ("redirect_uri", redirect_uri),
    ];
    let resp = client
        .post(&cfg.token_uri)
        .form(&body)
        .send()
        .await
        .map_err(|e| format!("oidc token http: {e}"))?;
    resp.json::<Value>()
        .await
        .map_err(|e| format!("oidc token json: {e}"))
}

/// ⑥⑦ GET `thirdUri?access_token=...&client_id=...`
pub async fn fetch_user_employee_id(
    client: &Client,
    cfg: &FourAConfig,
    access_token: &str,
) -> Result<String, String> {
    let mut url = reqwest::Url::parse(&cfg.third_uri).map_err(|e| format!("bad thirdUri: {e}"))?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("access_token", access_token);
        q.append_pair("client_id", &cfg.client_id);
    }

    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("4A userinfo http: {e}"))?;

    let v = resp
        .json::<Value>()
        .await
        .map_err(|e| format!("4A userinfo json: {e}"))?;

    if let Some(ec) = v.get("errcode") {
        let bad = match ec {
            Value::String(s) => !s.is_empty() && s != "0",
            Value::Number(n) => n.as_i64() != Some(0),
            _ => false,
        };
        if bad {
            let msg = v
                .get("msg")
                .and_then(|x| x.as_str())
                .unwrap_or("4A userinfo error");
            return Err(msg.to_string());
        }
    }

    v.get("employeeNumber")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "missing employeeNumber".to_string())
}

pub async fn fetch_oidc_userinfo(
    client: &Client,
    cfg: &OidcConfig,
    access_token: &str,
) -> Result<Value, String> {
    let resp = client
        .get(&cfg.userinfo_uri)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| format!("oidc userinfo http: {e}"))?;
    resp.json::<Value>()
        .await
        .map_err(|e| format!("oidc userinfo json: {e}"))
}

pub fn authorize_url_with_redirect(
    cfg: &FourAConfig,
    state: &str,
    redirect_uri: &str,
) -> Result<String, String> {
    let u = reqwest::Url::parse(&cfg.first_uri).map_err(|e| e.to_string())?;
    let mut u = u;
    {
        let mut q = u.query_pairs_mut();
        q.append_pair("response_type", "code");
        q.append_pair("client_id", &cfg.client_id);
        q.append_pair("redirect_uri", redirect_uri);
        q.append_pair("state", state);
    }
    Ok(u.to_string())
}

pub fn authorize_url_with_redirect_oidc(
    cfg: &OidcConfig,
    state: &str,
    redirect_uri: &str,
) -> Result<String, String> {
    let u = reqwest::Url::parse(&cfg.authorize_uri).map_err(|e| e.to_string())?;
    let mut u = u;
    {
        let mut q = u.query_pairs_mut();
        q.append_pair("response_type", "code");
        q.append_pair("client_id", &cfg.client_id);
        q.append_pair("redirect_uri", redirect_uri);
        q.append_pair("scope", &cfg.scopes);
        q.append_pair("state", state);
    }
    Ok(u.to_string())
}

pub fn extract_groups(v: &Value) -> Vec<String> {
    let mut groups = Vec::new();
    if let Some(arr) = v.get("groups").and_then(|x| x.as_array()) {
        for g in arr {
            if let Some(s) = g.as_str() {
                let t = s.trim();
                if !t.is_empty() {
                    groups.push(t.to_string());
                }
            }
        }
    } else if let Some(s) = v.get("groups").and_then(|x| x.as_str()) {
        for p in s.split(',') {
            let t = p.trim();
            if !t.is_empty() {
                groups.push(t.to_string());
            }
        }
    }
    groups.sort();
    groups.dedup();
    groups
}
