#[derive(Clone)]
pub struct EmailSender {
    http: reqwest::Client,
    api_key: String,
    from: String,
}

impl EmailSender {
    pub fn new(http: reqwest::Client, api_key: String, from: String) -> Self {
        Self {
            http,
            api_key,
            from,
        }
    }

    /// `code` — when non-empty, displayed prominently for manual entry.
    ///          Leave empty when only the confirmation link is needed.
    pub async fn send_confirmation(
        &self,
        to: &str,
        name: &str,
        code: &str,
        confirm_url: &str,
        locale: &str,
    ) -> anyhow::Result<()> {
        let code_block = if code.is_empty() {
            String::new()
        } else if locale == "ko" {
            format!("<p>인증 코드: <strong style=\"font-size:1.5em;letter-spacing:0.15em\">{code}</strong></p>")
        } else {
            format!("<p>Your confirmation code: <strong style=\"font-size:1.5em;letter-spacing:0.15em\">{code}</strong></p>")
        };

        let (subject, body) = if locale == "ko" {
            (
                "이메일 주소를 인증해 주세요".to_string(),
                format!(
                    "<p>안녕하세요 {name},</p>\
                     {code_block}\
                     <p>또는 아래 링크를 클릭하여 자동으로 인증하세요.</p>\
                     <p><a href=\"{confirm_url}\">{confirm_url}</a></p>"
                ),
            )
        } else {
            (
                "Confirm your email address".to_string(),
                format!(
                    "<p>Hi {name},</p>\
                     {code_block}\
                     <p>Or click the link below to confirm automatically.</p>\
                     <p><a href=\"{confirm_url}\">{confirm_url}</a></p>"
                ),
            )
        };

        self.send(to, &subject, &body).await
    }

    pub async fn send_password_reset(
        &self,
        to: &str,
        name: &str,
        reset_url: &str,
        locale: &str,
    ) -> anyhow::Result<()> {
        let (subject, body) = if locale == "ko" {
            (
                "비밀번호 재설정".to_string(),
                format!(
                    "<p>안녕하세요 {name},</p>\
                     <p>아래 링크를 클릭하여 비밀번호를 재설정하세요. 이 링크는 1시간 동안 유효합니다.</p>\
                     <p><a href=\"{reset_url}\">{reset_url}</a></p>\
                     <p>비밀번호 재설정을 요청하지 않으셨다면 이 메일을 무시하세요.</p>"
                ),
            )
        } else {
            (
                "Reset your password".to_string(),
                format!(
                    "<p>Hi {name},</p>\
                     <p>Click the link below to reset your password. This link expires in 1 hour.</p>\
                     <p><a href=\"{reset_url}\">{reset_url}</a></p>\
                     <p>If you did not request a password reset, ignore this email.</p>"
                ),
            )
        };

        self.send(to, &subject, &body).await
    }

    pub async fn send_notification(
        &self,
        to: &str,
        name: &str,
        notification_type: &str,
        actor: &str,
        instance_url: &str,
        locale: &str,
    ) -> anyhow::Result<()> {
        let (subject, body) = match (locale, notification_type) {
            ("ko", "mention") => (
                format!("{actor}님이 회원님을 멘션했습니다"),
                format!("<p>안녕하세요 {name},</p><p><strong>{actor}</strong>님이 게시물에서 회원님을 멘션했습니다.</p><p><a href=\"{instance_url}\">{instance_url}</a>에서 확인하세요.</p>"),
            ),
            ("ko", "follow") => (
                format!("{actor}님이 회원님을 팔로우했습니다"),
                format!("<p>안녕하세요 {name},</p><p><strong>{actor}</strong>님이 회원님을 팔로우하기 시작했습니다.</p><p><a href=\"{instance_url}\">{instance_url}</a>에서 확인하세요.</p>"),
            ),
            ("ko", "favourite") => (
                format!("{actor}님이 회원님의 게시물을 좋아합니다"),
                format!("<p>안녕하세요 {name},</p><p><strong>{actor}</strong>님이 회원님의 게시물을 즐겨찾기했습니다.</p><p><a href=\"{instance_url}\">{instance_url}</a>에서 확인하세요.</p>"),
            ),
            ("ko", "reblog") => (
                format!("{actor}님이 회원님의 게시물을 부스트했습니다"),
                format!("<p>안녕하세요 {name},</p><p><strong>{actor}</strong>님이 회원님의 게시물을 부스트했습니다.</p><p><a href=\"{instance_url}\">{instance_url}</a>에서 확인하세요.</p>"),
            ),
            (_, "mention") => (
                format!("{actor} mentioned you"),
                format!("<p>Hi {name},</p><p><strong>{actor}</strong> mentioned you in a post.</p><p>Visit <a href=\"{instance_url}\">{instance_url}</a> to see it.</p>"),
            ),
            (_, "follow") => (
                format!("{actor} followed you"),
                format!("<p>Hi {name},</p><p><strong>{actor}</strong> started following you.</p><p>Visit <a href=\"{instance_url}\">{instance_url}</a> to see their profile.</p>"),
            ),
            (_, "favourite") => (
                format!("{actor} liked your post"),
                format!("<p>Hi {name},</p><p><strong>{actor}</strong> favourited your post.</p><p>Visit <a href=\"{instance_url}\">{instance_url}</a> to see it.</p>"),
            ),
            (_, "reblog") => (
                format!("{actor} boosted your post"),
                format!("<p>Hi {name},</p><p><strong>{actor}</strong> boosted your post.</p><p>Visit <a href=\"{instance_url}\">{instance_url}</a> to see it.</p>"),
            ),
            _ => return Ok(()),
        };

        self.send(to, &subject, &body).await
    }

    /// Tell an administrator that newer Mastodon releases exist than the one
    /// this build implements.
    ///
    /// Mastodon's `AdminMailer#new_software_updates` and
    /// `#new_critical_software_updates`. The wording differs because the
    /// subject is not eunha's own version: eunha reproduces a Mastodon
    /// release's schema and API, and it is that release which has been
    /// superseded.
    pub async fn send_software_updates(
        &self,
        to: &str,
        name: &str,
        instance_domain: &str,
        tracked_version: &str,
        versions: &[String],
        urgent: bool,
    ) -> anyhow::Result<()> {
        let listed = versions
            .iter()
            .map(|v| format!("<li>Mastodon {v}</li>"))
            .collect::<String>();

        let subject = if urgent {
            format!("[{instance_domain}] Critical Mastodon updates available")
        } else {
            format!("[{instance_domain}] Mastodon updates available")
        };

        let urgency = if urgent {
            "<p><strong>At least one of these is marked urgent.</strong></p>"
        } else {
            ""
        };

        let body = format!(
            "<p>Hi {name},</p>\
             <p>{instance_domain} runs eunha, which implements Mastodon \
             {tracked_version}. Newer Mastodon releases are available:</p>\
             <ul>{listed}</ul>{urgency}\
             <p>Adopting one of them means a newer eunha, not a Mastodon \
             upgrade. Nothing here updates itself.</p>"
        );

        self.send(to, &subject, &body).await
    }

    /// Tell an administrator that the Mastodon release this build implements is
    /// losing, or has lost, upstream support.
    ///
    /// Mastodon's `AdminMailer#end_of_support_*`. `days_remaining` is negative
    /// once the date has passed.
    pub async fn send_end_of_support(
        &self,
        to: &str,
        name: &str,
        instance_domain: &str,
        branch: &str,
        end_of_support: &str,
        days_remaining: i64,
    ) -> anyhow::Result<()> {
        let (subject, urgency) = if days_remaining < 0 {
            (
                format!("[{instance_domain}] Mastodon {branch} is out of support"),
                format!(
                    "<p>Support for Mastodon {branch} ended on {end_of_support}. It no \
                     longer receives fixes, including security fixes.</p>"
                ),
            )
        } else {
            (
                format!("[{instance_domain}] Mastodon {branch} loses support soon"),
                format!(
                    "<p>Support for Mastodon {branch} ends on {end_of_support}, in \
                     {days_remaining} days.</p>"
                ),
            )
        };

        let body = format!(
            "<p>Hi {name},</p>\
             <p>{instance_domain} runs eunha, which implements Mastodon \
             {branch}.</p>{urgency}\
             <p>An eunha that tracks a supported Mastodon release is the way \
             out of this; see the project's release notes.</p>"
        );

        self.send(to, &subject, &body).await
    }

    async fn send(&self, to: &str, subject: &str, html: &str) -> anyhow::Result<()> {
        let payload = serde_json::json!({
            "from": self.from,
            "to": [to],
            "subject": subject,
            "html": html,
        });
        let resp = self
            .http
            .post("https://api.resend.com/emails")
            .bearer_auth(&self.api_key)
            .json(&payload)
            .send()
            .await?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Resend API error: {text}");
        }
        Ok(())
    }
}
