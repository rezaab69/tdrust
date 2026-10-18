//! Logging in.
//!
//! [`Client::authorize`] drives TDLib's authorization state machine and asks
//! an [`Authenticator`] for whatever it needs: a phone number or bot token,
//! the login code, the 2FA password, and so on. tdrust ships two
//! authenticators, [`BotAuthenticator`] and [`ConsoleAuthenticator`];
//! implement the trait to log in from a GUI, a web form or a secret store.

use std::future::Future;

use tokio::sync::oneshot;

use crate::types::{
    AuthenticationCodeInfo, AuthorizationState, AuthorizationStateWaitEmailCode, AuthorizationStateWaitPassword,
    EmailAddressAuthenticationCode, TermsOfService,
};
use crate::{Client, Error, Result, TdError};

/// How to log in when TDLib asks for credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Login {
    /// Log in as a user; TDLib then sends a login code to this number.
    PhoneNumber(String),
    /// Log in as a bot with a token from @BotFather.
    BotToken(String),
    /// Log in by scanning a QR code with an already logged-in device.
    /// [`Authenticator::qr_code_link`] receives the link to encode.
    QrCode,
}

/// The step of the login flow an error occurred in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AuthStep {
    /// Submitting a phone number or bot token, or requesting a QR code.
    Login,
    /// Submitting the login code.
    Code,
    /// Submitting the 2FA password.
    Password,
    /// Registering a new account.
    Registration,
    /// Submitting an email address.
    EmailAddress,
    /// Submitting an email code.
    EmailCode,
}

/// Name for a new account.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Registration {
    /// First name; 1-64 characters.
    pub first_name: String,
    /// Last name; 0-64 characters.
    pub last_name: String,
}

/// Supplies credentials during [`Client::authorize`].
///
/// Only [`login`](Authenticator::login) is required. The other methods are
/// called only when TDLib needs them, and fail by default.
///
/// ```
/// use tdrust::{Authenticator, Login, Result};
/// use tdrust::types::AuthenticationCodeInfo;
///
/// struct FromEnv;
///
/// impl Authenticator for FromEnv {
///     async fn login(&mut self) -> Result<Login> {
///         Ok(Login::PhoneNumber(std::env::var("PHONE").unwrap_or_default()))
///     }
///
///     async fn code(&mut self, _info: &AuthenticationCodeInfo) -> Result<String> {
///         Ok(std::env::var("LOGIN_CODE").unwrap_or_default())
///     }
/// }
/// ```
pub trait Authenticator: Send {
    /// How to log in.
    fn login(&mut self) -> impl Future<Output = Result<Login>> + Send;

    /// The login code TDLib sent as described by `info`.
    fn code(&mut self, info: &AuthenticationCodeInfo) -> impl Future<Output = Result<String>> + Send {
        let _ = info;
        async { Err(unsupported("a login code")) }
    }

    /// The account's two-step verification password.
    fn password(&mut self, state: &AuthorizationStateWaitPassword) -> impl Future<Output = Result<String>> + Send {
        let _ = state;
        async { Err(unsupported("a 2FA password")) }
    }

    /// Name for a new account, after the user has accepted `terms`.
    fn registration(&mut self, terms: &TermsOfService) -> impl Future<Output = Result<Registration>> + Send {
        let _ = terms;
        async { Err(unsupported("registration of a new account")) }
    }

    /// An email address to receive login codes.
    fn email_address(&mut self) -> impl Future<Output = Result<String>> + Send {
        async { Err(unsupported("an email address")) }
    }

    /// The code sent to the email address.
    fn email_code(&mut self, state: &AuthorizationStateWaitEmailCode) -> impl Future<Output = Result<String>> + Send {
        let _ = state;
        async { Err(unsupported("an email code")) }
    }

    /// Called with a `tg://login?token=...` link to show as a QR code when
    /// logging in with [`Login::QrCode`]. The link is refreshed periodically,
    /// and this method is called again each time.
    fn qr_code_link(&mut self, link: &str) -> impl Future<Output = Result<()>> + Send {
        log::info!("scan this link as a QR code with a logged-in Telegram app: {link}");
        async { Ok(()) }
    }

    /// Called when TDLib rejects what was submitted in `step` (for example
    /// `PHONE_CODE_INVALID`). Return `Ok(())` to be asked again, or an error
    /// to abort. By default, the error aborts the login.
    fn on_error(&mut self, step: AuthStep, error: &TdError) -> impl Future<Output = Result<()>> + Send {
        let _ = step;
        let error = error.clone();
        async move { Err(Error::Td(error)) }
    }
}

fn unsupported(what: &str) -> Error {
    Error::Authorization(format!("TDLib asked for {what}, which this authenticator does not provide"))
}

/// Logs in as a bot.
///
/// ```no_run
/// # async fn run(client: tdrust::Client) -> tdrust::Result<()> {
/// client.authorize(tdrust::BotAuthenticator::new("123456:ABC-DEF")).await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct BotAuthenticator {
    token: String,
}

impl BotAuthenticator {
    /// A bot token from @BotFather.
    pub fn new(token: impl Into<String>) -> Self {
        BotAuthenticator { token: token.into() }
    }
}

impl std::fmt::Debug for BotAuthenticator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BotAuthenticator").field("token", &"<redacted>").finish()
    }
}

impl Authenticator for BotAuthenticator {
    async fn login(&mut self) -> Result<Login> {
        Ok(Login::BotToken(self.token.clone()))
    }
}

/// Logs in interactively in the terminal.
///
/// Asks for a phone number or bot token, then for the login code, the 2FA
/// password (hidden when the `console` feature is enabled) and, for new
/// accounts, a name. Rejected input is reported and asked for again.
#[derive(Debug, Clone, Default)]
pub struct ConsoleAuthenticator {
    login: Option<Login>,
}

impl ConsoleAuthenticator {
    /// Asks for everything interactively.
    pub fn new() -> Self {
        Self::default()
    }

    /// Uses `phone_number` instead of asking for it the first time.
    pub fn with_phone_number(phone_number: impl Into<String>) -> Self {
        ConsoleAuthenticator { login: Some(Login::PhoneNumber(phone_number.into())) }
    }
}

impl Authenticator for ConsoleAuthenticator {
    async fn login(&mut self) -> Result<Login> {
        if let Some(login) = self.login.take() {
            return Ok(login);
        }
        let input = prompt("Phone number in international format (or a bot token): ".into(), false).await?;
        // Bot tokens look like `123456:ABC-DEF...`; phone numbers never contain `:`.
        Ok(if input.contains(':') { Login::BotToken(input) } else { Login::PhoneNumber(input) })
    }

    async fn code(&mut self, info: &AuthenticationCodeInfo) -> Result<String> {
        prompt(format!("Login code sent to {}: ", info.phone_number), false).await
    }

    async fn password(&mut self, state: &AuthorizationStateWaitPassword) -> Result<String> {
        let hint =
            if state.password_hint.is_empty() { String::new() } else { format!(" (hint: {})", state.password_hint) };
        prompt(format!("2FA password{hint}: "), true).await
    }

    async fn registration(&mut self, terms: &TermsOfService) -> Result<Registration> {
        println!("This phone number has no Telegram account yet. Terms of service:\n\n{}\n", terms.text.text);
        let first_name = prompt("First name: ".into(), false).await?;
        let last_name = prompt_optional("Last name (optional): ".into()).await?;
        Ok(Registration { first_name, last_name })
    }

    async fn email_address(&mut self) -> Result<String> {
        prompt("Email address for login codes: ".into(), false).await
    }

    async fn email_code(&mut self, _state: &AuthorizationStateWaitEmailCode) -> Result<String> {
        prompt("Code sent to your email: ".into(), false).await
    }

    async fn qr_code_link(&mut self, link: &str) -> Result<()> {
        println!("Open Telegram on a logged-in device, go to Settings > Devices > Link Desktop Device,");
        println!("and scan a QR code of this link: {link}");
        Ok(())
    }

    async fn on_error(&mut self, _step: AuthStep, error: &TdError) -> Result<()> {
        eprintln!("Telegram rejected that: {}", error.message);
        Ok(())
    }
}

/// Reads a non-empty line from the terminal without blocking the async
/// runtime.
async fn prompt(message: String, hidden: bool) -> Result<String> {
    read_line_in_thread(move || loop {
        let line = read_line(&message, hidden)?;
        if !line.is_empty() {
            return Ok(line);
        }
    })
    .await
}

async fn prompt_optional(message: String) -> Result<String> {
    read_line_in_thread(move || read_line(&message, false)).await
}

async fn read_line_in_thread(read: impl FnOnce() -> Result<String> + Send + 'static) -> Result<String> {
    let (sender, receiver) = oneshot::channel();
    std::thread::spawn(move || {
        let _ = sender.send(read());
    });
    receiver.await.map_err(|_| Error::Authorization("reading from the terminal failed".into()))?
}

fn read_line(message: &str, hidden: bool) -> Result<String> {
    use std::io::Write as _;

    #[cfg(feature = "console")]
    if hidden {
        return Ok(rpassword::prompt_password(message)?.trim().to_owned());
    }
    let _ = hidden;

    print!("{message}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line)? == 0 {
        return Err(Error::Authorization("the terminal input was closed".into()));
    }
    Ok(line.trim().to_owned())
}

/// Submits an answer, handing TDLib errors to the authenticator until it
/// accepts or gives up.
macro_rules! attempt {
    ($authenticator:ident, $step:expr, $body:expr) => {
        loop {
            match $body {
                Ok(_) => break,
                Err(Error::Td(error)) => $authenticator.on_error($step, &error).await?,
                Err(other) => return Err(other),
            }
        }
    };
}

impl Client {
    /// Logs in, asking `authenticator` for credentials as needed, and returns
    /// once the client is ready to use.
    ///
    /// If the database directory holds a session from an earlier run, this
    /// returns without asking for anything.
    ///
    /// ```no_run
    /// # async fn run() -> tdrust::Result<()> {
    /// let (client, _updates) = tdrust::Client::builder().build()?;
    /// client.authorize(tdrust::ConsoleAuthenticator::new()).await?;
    /// let me = client.get_me().await?;
    /// println!("Logged in as {}", me.first_name);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn authorize<A: Authenticator>(&self, mut authenticator: A) -> Result<()> {
        let mut states = self.auth_receiver();
        loop {
            let state = states.borrow_and_update().clone();
            if let Some(state) = state {
                match state {
                    AuthorizationState::Ready(_) => return Ok(()),
                    AuthorizationState::WaitTdlibParameters(_) => {
                        self.send(self.parameters().clone()).await?;
                    }
                    AuthorizationState::WaitPhoneNumber(_) => attempt!(authenticator, AuthStep::Login, {
                        match authenticator.login().await? {
                            Login::PhoneNumber(number) => {
                                self.set_authentication_phone_number(number, None).await.map(drop)
                            }
                            Login::BotToken(token) => self.check_authentication_bot_token(token).await.map(drop),
                            Login::QrCode => self.request_qr_code_authentication(Vec::new()).await.map(drop),
                        }
                    }),
                    AuthorizationState::WaitCode(state) => attempt!(authenticator, AuthStep::Code, {
                        let code = authenticator.code(&state.code_info).await?;
                        self.check_authentication_code(code).await
                    }),
                    AuthorizationState::WaitPassword(state) => attempt!(authenticator, AuthStep::Password, {
                        let password = authenticator.password(&state).await?;
                        self.check_authentication_password(password).await
                    }),
                    AuthorizationState::WaitRegistration(state) => attempt!(authenticator, AuthStep::Registration, {
                        let name = authenticator.registration(&state.terms_of_service).await?;
                        self.register_user(name.first_name, name.last_name, false).await
                    }),
                    AuthorizationState::WaitEmailAddress(_) => attempt!(authenticator, AuthStep::EmailAddress, {
                        let email = authenticator.email_address().await?;
                        self.set_authentication_email_address(email).await
                    }),
                    AuthorizationState::WaitEmailCode(state) => attempt!(authenticator, AuthStep::EmailCode, {
                        let code = authenticator.email_code(&state).await?;
                        self.check_authentication_email_code(EmailAddressAuthenticationCode { code }).await
                    }),
                    AuthorizationState::WaitOtherDeviceConfirmation(state) => {
                        authenticator.qr_code_link(&state.link).await?;
                    }
                    AuthorizationState::LoggingOut(_)
                    | AuthorizationState::Closing(_)
                    | AuthorizationState::Closed(_) => return Err(Error::ClientClosed(self.id())),
                    other => {
                        return Err(Error::Authorization(format!(
                            "TDLib is waiting for `{}`, which tdrust cannot complete; handle it with the \
                             generated API and `Client::authorization_state`",
                            other.type_name()
                        )))
                    }
                }
            }
            if states.changed().await.is_err() {
                return Err(Error::ClientClosed(self.id()));
            }
        }
    }
}
