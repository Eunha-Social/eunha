/// Two-locale support (en / ko) for server-rendered pages.

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Locale {
    En,
    Ko,
}

impl Locale {
    /// Resolve locale: explicit `?lang=` param wins, then first matching tag
    /// in the `Accept-Language` header, then English.
    pub fn detect(lang_param: Option<&str>, accept_language: Option<&str>) -> Self {
        if let Some(p) = lang_param {
            match p.to_lowercase().as_str() {
                "ko" => return Self::Ko,
                "en" => return Self::En,
                _ => {}
            }
        }
        if let Some(al) = accept_language {
            for tag in al.split(',') {
                let lang = tag.trim().split(';').next().unwrap_or("").trim();
                if lang.starts_with("ko") {
                    return Self::Ko;
                }
                if lang.starts_with("en") {
                    return Self::En;
                }
            }
        }
        Self::En
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Ko => "ko",
        }
    }

    pub fn t(self, key: &str) -> &'static str {
        match (self, key) {
            // ── authorize page ───────────────────────────────────────────────
            (Self::En, "sign_in_to") => "Sign in to",
            (Self::Ko, "sign_in_to") => "로그인",
            (Self::En, "authorize") => "Authorize",
            (Self::Ko, "authorize") => "인증",
            (Self::En, "email") => "Email",
            (Self::Ko, "email") => "이메일",
            (Self::En, "password") => "Password",
            (Self::Ko, "password") => "비밀번호",
            (Self::En, "sign_in") => "Sign in",
            (Self::Ko, "sign_in") => "로그인",
            (Self::En, "invalid_credentials") => "Invalid email or password.",
            (Self::Ko, "invalid_credentials") => "이메일 또는 비밀번호가 올바르지 않습니다.",
            (Self::En, "check_email") => {
                "Check your email and click the confirmation link to activate your account."
            }
            (Self::Ko, "check_email") => {
                "이메일을 확인하여 인증 링크를 클릭해 계정을 활성화하세요."
            }
            (Self::En, "no_account") => "Don't have an account?",
            (Self::Ko, "no_account") => "계정이 없으신가요?",
            (Self::En, "sign_up") => "Sign up",
            (Self::Ko, "sign_up") => "가입하기",
            // ── signup page ──────────────────────────────────────────────────
            (Self::En, "create_account") => "Create account",
            (Self::Ko, "create_account") => "계정 만들기",
            (Self::En, "username") => "Username",
            (Self::Ko, "username") => "사용자 이름",
            (Self::En, "confirm_password") => "Confirm password",
            (Self::Ko, "confirm_password") => "비밀번호 확인",
            (Self::En, "already_account") => "Already have an account?",
            (Self::Ko, "already_account") => "이미 계정이 있으신가요?",
            (Self::En, "registrations_closed") => "Registrations are currently by invite only.",
            (Self::Ko, "registrations_closed") => "이 인스턴스는 초대로만 가입할 수 있습니다.",
            (Self::En, "invite_code") => "Invite code",
            (Self::Ko, "invite_code") => "초대 코드",
            (Self::En, "continue_btn") => "Continue",
            (Self::Ko, "continue_btn") => "계속",
            // ── signup error messages ────────────────────────────────────────
            (Self::En, "err_invite_required") => "An invite code is required.",
            (Self::Ko, "err_invite_required") => "초대 코드가 필요합니다.",
            (Self::En, "err_invalid_invite") => "Invalid invite code.",
            (Self::Ko, "err_invalid_invite") => "유효하지 않은 초대 코드입니다.",
            (Self::En, "err_invite_maxed") => "This invite has reached its use limit.",
            (Self::Ko, "err_invite_maxed") => "이 초대는 사용 횟수를 초과했습니다.",
            (Self::En, "err_invite_expired") => "This invite has expired.",
            (Self::Ko, "err_invite_expired") => "이 초대는 만료되었습니다.",
            (Self::En, "err_username_chars") => {
                "Username may only contain letters, numbers, and underscores."
            }
            (Self::Ko, "err_username_chars") => {
                "사용자 이름은 영문자, 숫자, 밑줄만 사용할 수 있습니다."
            }
            (Self::En, "err_invalid_email") => "Enter a valid email address.",
            (Self::Ko, "err_invalid_email") => "유효한 이메일 주소를 입력해주세요.",
            (Self::En, "err_password_short") => "Password must be at least 8 characters.",
            (Self::Ko, "err_password_short") => "비밀번호는 8자 이상이어야 합니다.",
            (Self::En, "err_password_mismatch") => "Passwords do not match.",
            (Self::Ko, "err_password_mismatch") => "비밀번호가 일치하지 않습니다.",
            (Self::En, "err_username_taken") => "That username is already taken.",
            (Self::Ko, "err_username_taken") => "이미 사용 중인 사용자 이름입니다.",
            (Self::En, "err_email_taken") => "An account with that email already exists.",
            (Self::Ko, "err_email_taken") => "이미 사용 중인 이메일입니다.",
            (Self::En, "err_server") => "Server error. Please try again.",
            (Self::Ko, "err_server") => "서버 오류가 발생했습니다. 다시 시도해주세요.",
            // ── account pages ────────────────────────────────────────────────
            (Self::En, "account") => "Account",
            (Self::Ko, "account") => "계정",
            (Self::En, "invite_tree") => "Invite tree",
            (Self::Ko, "invite_tree") => "초대 트리",
            (Self::En, "change_password") => "Change password",
            (Self::Ko, "change_password") => "비밀번호 변경",
            (Self::En, "current_password") => "Current password",
            (Self::Ko, "current_password") => "현재 비밀번호",
            (Self::En, "new_password") => "New password",
            (Self::Ko, "new_password") => "새 비밀번호",
            (Self::En, "confirm_new_password") => "Confirm new password",
            (Self::Ko, "confirm_new_password") => "새 비밀번호 확인",
            (Self::En, "password_mismatch") => "New passwords do not match.",
            (Self::Ko, "password_mismatch") => "새 비밀번호가 일치하지 않습니다.",
            (Self::En, "sign_out") => "Sign out",
            (Self::Ko, "sign_out") => "로그아웃",
            (Self::En, "go_to_timeline") => "Go to timeline",
            (Self::Ko, "go_to_timeline") => "타임라인으로 돌아가기",
            (Self::En, "back_to_account") => "← Account",
            (Self::Ko, "back_to_account") => "← 계정",
            (Self::En, "password_changed") => "Password changed.",
            (Self::Ko, "password_changed") => "비밀번호가 변경되었습니다.",
            (Self::En, "password_error") => "Failed. Check your current password.",
            (Self::Ko, "password_error") => "실패했습니다. 현재 비밀번호를 확인해 주세요.",
            (Self::En, "no_members") => "No members yet.",
            (Self::Ko, "no_members") => "아직 멤버가 없습니다.",
            (Self::En, "uninvited_members") => "Members without invite",
            (Self::Ko, "uninvited_members") => "초대 없이 가입한 멤버",
            (Self::En, "expired") => "expired",
            (Self::Ko, "expired") => "만료됨",
            // ── signup approval ──────────────────────────────────────────────
            (Self::En, "reason") => "Why do you want to join?",
            (Self::Ko, "reason") => "가입 이유를 알려주세요",
            (Self::En, "reason_hint") => "Tell us a bit about yourself.",
            (Self::Ko, "reason_hint") => "간단히 소개해 주세요.",
            (Self::En, "pending_approval") => {
                "Your account is pending approval. You will be notified once approved."
            }
            (Self::Ko, "pending_approval") => "계정 승인 대기 중입니다. 승인되면 알려드리겠습니다.",
            (Self::En, "apply_for_account") => "Apply for an account",
            (Self::Ko, "apply_for_account") => "계정 신청하기",
            // fallback
            _ => "",
        }
    }
}
