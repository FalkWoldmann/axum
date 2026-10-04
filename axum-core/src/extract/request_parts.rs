use super::{rejection::*, FromRequest, FromRequestParts, Request};
use crate::{body::Body, ext_traits::request::body_limit, RequestExt};
use bytes::{BufMut, Bytes, BytesMut};
use http::{request::Parts, Extensions, HeaderMap, Method, Uri, Version};
use http_body_util::{BodyExt, Limited};
use std::convert::Infallible;

impl<S> FromRequest<S> for Request
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request(req: Request, _: &S) -> Result<Self, Self::Rejection> {
        Ok(req)
    }
}

impl<S> FromRequestParts<S> for Method
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(parts.method.clone())
    }
}

impl<S> FromRequestParts<S> for Uri
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(parts.uri.clone())
    }
}

impl<S> FromRequestParts<S> for Version
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(parts.version)
    }
}

/// Clone the headers from the request.
///
/// Prefer using [`TypedHeader`] to extract only the headers you need.
///
/// [`TypedHeader`]: https://docs.rs/axum-extra/0.10/axum_extra/struct.TypedHeader.html
impl<S> FromRequestParts<S> for HeaderMap
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(parts.headers.clone())
    }
}

#[diagnostic::do_not_recommend] // pretty niche impl
impl<S> FromRequest<S> for BytesMut
where
    S: Send + Sync,
{
    type Rejection = BytesRejection;

    async fn from_request(req: Request, _: &S) -> Result<Self, Self::Rejection> {
        let mut body = req.into_limited_body();
        #[allow(clippy::use_self)]
        let mut bytes = BytesMut::new();
        body_to_bytes_mut(&mut body, &mut bytes).await?;
        Ok(bytes)
    }
}

async fn body_to_bytes_mut(body: &mut Body, bytes: &mut BytesMut) -> Result<(), BytesRejection> {
    while let Some(frame) = body
        .frame()
        .await
        .transpose()
        .map_err(FailedToBufferBody::from_err)?
    {
        let Ok(data) = frame.into_data() else {
            // Match `Bytes` by ignoring non-data frames until the body ends.
            continue;
        };
        bytes.put(data);
    }

    Ok(())
}

impl<S> FromRequest<S> for Bytes
where
    S: Send + Sync,
{
    type Rejection = BytesRejection;

    async fn from_request(req: Request, _: &S) -> Result<Self, Self::Rejection> {
        // `into_limited_body` would box the body
        let bytes = match body_limit(&req) {
            Some(limit) => collect_bytes(Limited::new(req.into_body(), limit)).await,
            None => collect_bytes(req.into_body()).await.map_err(Into::into),
        }
        .map_err(FailedToBufferBody::from_err)?;

        Ok(bytes)
    }
}

/// Like `BodyExt::collect`, but without copying a body that consists of a single frame.
async fn collect_bytes<B>(mut body: B) -> Result<Bytes, B::Error>
where
    B: http_body::Body<Data = Bytes> + Unpin,
{
    let mut first = None;
    let mut buf = BytesMut::new();
    while let Some(frame) = body.frame().await.transpose()? {
        let data = match frame.into_data() {
            Ok(data) if !data.is_empty() => data,
            _ => continue,
        };
        match first.take() {
            None if buf.is_empty() => first = Some(data),
            first => {
                buf.extend(first);
                buf.put(data);
            }
        }
    }
    Ok(first.unwrap_or_else(|| buf.freeze()))
}

impl<S> FromRequest<S> for String
where
    S: Send + Sync,
{
    type Rejection = StringRejection;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let bytes = Bytes::from_request(req, state)
            .await
            .map_err(|err| match err {
                BytesRejection::FailedToBufferBody(inner) => {
                    StringRejection::FailedToBufferBody(inner)
                }
            })?;

        #[allow(clippy::use_self)]
        let string = String::from_utf8(bytes.into()).map_err(InvalidUtf8::from_err)?;

        Ok(string)
    }
}

#[diagnostic::do_not_recommend] // pretty niche impl
impl<S> FromRequestParts<S> for Parts
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(parts.clone())
    }
}

#[diagnostic::do_not_recommend] // pretty niche impl
impl<S> FromRequestParts<S> for Extensions
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(parts.extensions.clone())
    }
}

impl<S> FromRequest<S> for Body
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request(req: Request, _: &S) -> Result<Self, Self::Rejection> {
        Ok(req.into_body())
    }
}

#[cfg(test)]
mod tests {
    use crate::extract::{
        rejection::{BytesRejection, FailedToBufferBody},
        DefaultBodyLimitKind, FromRequest, Request,
    };
    use axum::{extract::Extension, routing::get, test_helpers::*, Router};
    use bytes::Bytes;
    use http::{Method, StatusCode};
    use http_body::Frame;
    use std::{
        collections::VecDeque,
        convert::Infallible,
        pin::Pin,
        task::{Context, Poll},
    };

    #[crate::test]
    async fn extract_request_parts() {
        #[derive(Clone)]
        struct Ext;

        async fn handler(parts: http::request::Parts) {
            assert_eq!(parts.method, Method::GET);
            assert_eq!(parts.uri, "/");
            assert_eq!(parts.version, http::Version::HTTP_11);
            assert_eq!(parts.headers["x-foo"], "123");
            parts.extensions.get::<Ext>().unwrap();
        }

        let client = TestClient::new(Router::new().route("/", get(handler)).layer(Extension(Ext)));

        let res = client.get("/").header("x-foo", "123").await;
        assert_eq!(res.status(), StatusCode::OK);
    }

    struct Frames(VecDeque<&'static str>);

    impl http_body::Body for Frames {
        type Data = Bytes;
        type Error = Infallible;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
            let frame = self
                .0
                .pop_front()
                .map(|data| Ok(Frame::data(Bytes::from(data))));
            Poll::Ready(frame)
        }
    }

    async fn bytes(frames: &[&'static str], limit: Option<usize>) -> Result<Bytes, BytesRejection> {
        let mut req = Request::new(crate::body::Body::new(Frames(
            frames.iter().copied().collect(),
        )));
        if let Some(limit) = limit {
            req.extensions_mut()
                .insert(DefaultBodyLimitKind::Limit(limit));
        }
        Bytes::from_request(req, &()).await
    }

    #[crate::test]
    async fn bytes_from_single_frame_is_not_copied() {
        let data = "single frame";
        let bytes = bytes(&[data], None).await.unwrap();
        assert_eq!(bytes.as_ptr(), data.as_ptr());
    }

    #[crate::test]
    async fn bytes_from_many_frames() {
        assert_eq!(
            bytes(&["", "ab", "", "cd", "e"], None).await.unwrap(),
            "abcde"
        );
        assert_eq!(bytes(&["", ""], None).await.unwrap(), "");
        assert_eq!(bytes(&["ab", "cd"], Some(4)).await.unwrap(), "abcd");
        assert!(matches!(
            bytes(&["ab", "cd", "e"], Some(4)).await,
            Err(BytesRejection::FailedToBufferBody(
                FailedToBufferBody::LengthLimitError(_)
            ))
        ));
    }
}
