use crate::util::PercentDecodedStr;
use http::Extensions;
use matchit::Params;
use std::sync::{Arc, OnceLock};

#[derive(Clone)]
pub(crate) enum UrlParams {
    Params {
        params: Vec<(Arc<str>, PercentDecodedStr)>,
        inner_start: usize,
    },
    InvalidUtf8InPathParam {
        key: Arc<str>,
    },
}

pub(super) type ParamNames = Box<[Arc<str>]>;

fn is_user_param((key, _): &(&str, &str)) -> bool {
    !key.starts_with(super::NEST_TAIL_PARAM) && !key.starts_with(super::FALLBACK_PARAM)
}

pub(super) fn insert_url_params(
    extensions: &mut Extensions,
    params: &Params<'_, '_>,
    names: &OnceLock<ParamNames>,
) {
    let current_params = extensions.get_mut();

    if let Some(UrlParams::InvalidUtf8InPathParam { .. }) = current_params {
        // nothing to do here since an error was stored earlier
        return;
    }

    let names = names.get_or_init(|| {
        params
            .iter()
            .filter(is_user_param)
            .map(|(k, _)| Arc::from(k))
            .collect()
    });

    let params = params
        .iter()
        .filter(is_user_param)
        .zip(names.iter())
        .map(|((k, v), name)| {
            debug_assert_eq!(k, &**name);
            PercentDecodedStr::new(v)
                .map(|decoded| (Arc::clone(name), decoded))
                .ok_or_else(|| Arc::clone(name))
        })
        .collect::<Result<Vec<_>, _>>();

    match (current_params, params) {
        (Some(UrlParams::InvalidUtf8InPathParam { .. }), _) => {
            unreachable!("we check for this state earlier in this method")
        }
        (_, Err(invalid_key)) => {
            extensions.insert(UrlParams::InvalidUtf8InPathParam { key: invalid_key });
        }
        (
            Some(UrlParams::Params {
                params: current, ..
            }),
            Ok(params),
        ) => {
            current.extend(params);
        }
        (None, Ok(params)) => {
            extensions.insert(UrlParams::Params {
                params,
                inner_start: 0,
            });
        }
    }
}

pub(super) fn advance_inner_start(extensions: &mut Extensions, count: usize) {
    if let Some(UrlParams::Params { inner_start, .. }) = extensions.get_mut() {
        *inner_start += count;
    }
}
