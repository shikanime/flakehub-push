mod instrumentation;

use color_eyre::eyre::{eyre, WrapErr};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    process::ExitCode,
};

use crate::{
    build_http_client,
    flakehub_client::Tarball,
    github::graphql::{GithubGraphqlDataQuery, MAX_LABEL_LENGTH, MAX_NUM_TOTAL_LABELS},
    release_metadata::ReleaseMetadata,
};

#[derive(Debug, clap::Parser)]
#[clap(version)]
pub(crate) struct FlakeHubPushCli {
    #[clap(
        long,
        env = "FLAKEHUB_PUSH_HOST",
        default_value = "https://api.flakehub.com"
    )]
    pub(crate) host: url::Url,

    #[clap(long, env = "FLAKEHUB_PUSH_VISIBILITY")]
    pub(crate) visibility: Option<crate::Visibility>,
    // This was the original env var to set this value. As you can see, we previously misspelled it.
    // We need to continue to support it just in case.
    #[clap(long, env = "FLAKEHUB_PUSH_VISIBLITY")]
    pub(crate) visibility_alt: Option<crate::Visibility>,

    // Will also detect `GITHUB_REF_NAME`
    #[clap(long, env = "FLAKEHUB_PUSH_TAG", value_parser = StringToNoneParser, default_value = "")]
    pub(crate) tag: OptionString,
    #[clap(long, env = "FLAKEHUB_PUSH_ROLLING_MINOR", value_parser = U64ToNoneParser, default_value = "")]
    pub(crate) rolling_minor: OptionU64,
    #[clap(long, env = "FLAKEHUB_PUSH_ROLLING", value_parser = EmptyBoolParser, default_value_t = false)]
    pub(crate) rolling: bool,
    // Also detects `GITHUB_TOKEN`
    #[clap(long, env = "FLAKEHUB_PUSH_GITHUB_TOKEN", value_parser = StringToNoneParser, default_value = "")]
    pub(crate) github_token: OptionString,
    #[clap(long, env = "FLAKEHUB_PUSH_NAME", value_parser = StringToNoneParser, default_value = "")]
    pub(crate) name: OptionString,
    /// Will also detect `GITHUB_REPOSITORY`
    #[clap(long, env = "FLAKEHUB_PUSH_REPOSITORY", value_parser = StringToNoneParser, default_value = "")]
    pub(crate) repository: OptionString,
    // Also detects `GITHUB_WORKSPACE`
    #[clap(long, env = "FLAKEHUB_PUSH_DIRECTORY", value_parser = PathBufToNoneParser, default_value = "")]
    pub(crate) directory: OptionPathBuf,
    // Also detects `GITHUB_WORKSPACE`
    #[clap(long, env = "FLAKEHUB_PUSH_GIT_ROOT", value_parser = PathBufToNoneParser, default_value = "")]
    pub(crate) git_root: OptionPathBuf,
    // If the repository is mirrored via DeterminateSystems' mirror functionality
    //
    // This should only be used by DeterminateSystems
    #[clap(long, env = "FLAKEHUB_PUSH_MIRROR", default_value_t = false)]
    pub(crate) mirror: bool,
    /// URL of a JWT mock server (like https://github.com/ruiyang/jwt-mock-server) which can issue tokens.
    ///
    /// Used instead of ACTIONS_ID_TOKEN_REQUEST_URL/ACTIONS_ID_TOKEN_REQUEST_TOKEN when developing locally.
    #[clap(long, env = "FLAKEHUB_PUSH_JWT_ISSUER_URI", value_parser = StringToNoneParser, default_value = "")]
    pub(crate) jwt_issuer_uri: OptionString,

    /// User-supplied labels, merged with any associated with GitHub repository (if possible)
    #[clap(
        long,
        short = 'l',
        env = "FLAKEHUB_PUSH_EXTRA_LABELS",
        use_value_delimiter = true,
        value_delimiter = ','
    )]
    pub(crate) extra_labels: Vec<String>,

    /// DEPRECATED: Please use `extra-labels` instead.
    #[clap(
        long,
        short = 't',
        env = "FLAKEHUB_PUSH_EXTRA_TAGS",
        use_value_delimiter = true,
        value_delimiter = ','
    )]
    pub(crate) extra_tags: Vec<String>,

    /// An SPDX identifier from https://spdx.org/licenses/, inferred from GitHub (if possible)
    #[clap(
        long,
        env = "FLAKEHUB_PUSH_SPDX_EXPRESSION",
        value_parser = SpdxToNoneParser,
        default_value = ""
    )]
    pub(crate) spdx_expression: OptionSpdxExpression,

    #[clap(
        long,
        env = "FLAKEHUB_PUSH_ERROR_ON_CONFLICT",
        value_parser = EmptyBoolParser,
        default_value_t = false
    )]
    pub(crate) error_on_conflict: bool,

    #[clap(flatten)]
    pub instrumentation: instrumentation::Instrumentation,

    #[clap(long, env = "FLAKEHUB_PUSH_INCLUDE_OUTPUT_PATHS", value_parser = EmptyBoolParser, default_value_t = false)]
    pub(crate) include_output_paths: bool,
}

#[derive(Clone, Debug)]
pub struct OptionString(pub Option<String>);

#[derive(Clone)]
struct StringToNoneParser;

impl clap::builder::TypedValueParser for StringToNoneParser {
    type Value = OptionString;

    fn parse_ref(
        &self,
        cmd: &clap::Command,
        arg: Option<&clap::Arg>,
        value: &std::ffi::OsStr,
    ) -> Result<Self::Value, clap::Error> {
        let inner = clap::builder::StringValueParser::new();
        let val = inner.parse_ref(cmd, arg, value)?;

        if val.is_empty() {
            Ok(OptionString(None))
        } else {
            Ok(OptionString(Some(Into::<String>::into(val))))
        }
    }
}

#[derive(Clone, Debug)]
pub struct OptionPathBuf(pub Option<PathBuf>);

#[derive(Clone)]
struct PathBufToNoneParser;

impl clap::builder::TypedValueParser for PathBufToNoneParser {
    type Value = OptionPathBuf;

    fn parse_ref(
        &self,
        cmd: &clap::Command,
        arg: Option<&clap::Arg>,
        value: &std::ffi::OsStr,
    ) -> Result<Self::Value, clap::Error> {
        let inner = clap::builder::StringValueParser::new();
        let val = inner.parse_ref(cmd, arg, value)?;

        if val.is_empty() {
            Ok(OptionPathBuf(None))
        } else {
            Ok(OptionPathBuf(Some(Into::<PathBuf>::into(val))))
        }
    }
}

#[derive(Clone, Debug)]
pub struct OptionSpdxExpression(pub Option<spdx::Expression>);

#[derive(Clone)]
struct SpdxToNoneParser;

impl clap::builder::TypedValueParser for SpdxToNoneParser {
    type Value = OptionSpdxExpression;

    fn parse_ref(
        &self,
        cmd: &clap::Command,
        arg: Option<&clap::Arg>,
        value: &std::ffi::OsStr,
    ) -> Result<Self::Value, clap::Error> {
        let inner = clap::builder::StringValueParser::new();
        let val = inner.parse_ref(cmd, arg, value)?;

        if val.is_empty() {
            Ok(OptionSpdxExpression(None))
        } else {
            let expression = spdx::Expression::parse(&val).map_err(|e| {
                clap::Error::raw(clap::error::ErrorKind::ValueValidation, format!("{e}"))
            })?;
            Ok(OptionSpdxExpression(Some(expression)))
        }
    }
}

#[derive(Clone)]
struct EmptyBoolParser;

impl clap::builder::TypedValueParser for EmptyBoolParser {
    type Value = bool;

    fn parse_ref(
        &self,
        cmd: &clap::Command,
        arg: Option<&clap::Arg>,
        value: &std::ffi::OsStr,
    ) -> Result<Self::Value, clap::Error> {
        let inner = clap::builder::StringValueParser::new();
        let val = inner.parse_ref(cmd, arg, value)?;

        if val.is_empty() {
            Ok(false)
        } else {
            let val = match val.as_ref() {
                "true" => true,
                "false" => false,
                v => {
                    return Err(clap::Error::raw(
                        clap::error::ErrorKind::InvalidValue,
                        format!("`{v}` was not `true` or `false`\n"),
                    ))
                }
            };
            Ok(val)
        }
    }
}

#[derive(Clone, Debug)]
pub struct OptionU64(pub Option<u64>);

#[derive(Clone)]
struct U64ToNoneParser;

impl clap::builder::TypedValueParser for U64ToNoneParser {
    type Value = OptionU64;

    fn parse_ref(
        &self,
        cmd: &clap::Command,
        arg: Option<&clap::Arg>,
        value: &std::ffi::OsStr,
    ) -> Result<Self::Value, clap::Error> {
        let inner = clap::builder::StringValueParser::new();
        let val = inner.parse_ref(cmd, arg, value)?;

        if val.is_empty() {
            Ok(OptionU64(None))
        } else {
            let expression = val.parse::<u64>().map_err(|e| {
                clap::Error::raw(clap::error::ErrorKind::ValueValidation, format!("{e}\n"))
            })?;
            Ok(OptionU64(Some(expression)))
        }
    }
}

impl FlakeHubPushCli {
    pub(crate) fn backfill_from_github_env(&mut self) {
        // https://docs.github.com/en/actions/learn-github-actions/variables

        if self.git_root.0.is_none() {
            let env_key = "GITHUB_WORKSPACE";
            if let Ok(env_val) = std::env::var(env_key) {
                tracing::debug!(git_root = %env_val, "Set via `${env_key}`");
                self.git_root.0 = Some(PathBuf::from(env_val));
            }
        }

        if self.repository.0.is_none() {
            let env_key = "GITHUB_REPOSITORY";
            if let Ok(env_val) = std::env::var(env_key) {
                tracing::debug!(repository = %env_val, "Set via `${env_key}`");
                self.repository.0 = Some(env_val);
            }
        }

        if self.tag.0.is_none() {
            let env_key = "GITHUB_REF_NAME";
            if let Ok(env_val) = std::env::var(env_key) {
                tracing::debug!(repository = %env_val, "Set via `${env_key}`");
                self.tag.0 = Some(env_val);
            }
        }
    }

    pub(crate) fn backfill_from_gitlab_env(&mut self) {
        // https://docs.gitlab.com/ee/ci/variables/predefined_variables.html

        if self.git_root.0.is_none() {
            let env_key: &str = "CI_PROJECT_DIR";
            if let Ok(env_val) = std::env::var(env_key) {
                tracing::debug!(git_root = %env_val, "Set via `${env_key}`");
                self.git_root.0 = Some(PathBuf::from(env_val));
            }
        }

        if self.repository.0.is_none() {
            let env_key = "CI_PROJECT_ID";
            if let Ok(env_val) = std::env::var(env_key) {
                tracing::debug!(repository = %env_val, "Set via `${env_key}`");
                self.repository.0 = Some(env_val);
            }
        }

        // TODO(review): this... isn't really a "tag" for github either, but I think maybe that's intentional?
        if self.tag.0.is_none() {
            let env_key = "CI_COMMIT_REF_NAME";
            if let Ok(env_val) = std::env::var(env_key) {
                tracing::debug!(repository = %env_val, "Set via `${env_key}`");
                self.tag.0 = Some(env_val);
            }
        }
    }
}
