use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;

pub(crate) type Response = hyper::Response<Full<Bytes>>;
pub(crate) type Req = http::Request<Incoming>;

pub(crate) fn mk_resp(status: hyper::StatusCode, data: Option<impl Into<Bytes>>) -> Response {
    hyper::Response::builder()
        .status(status)
        .body(data.map(|b| Full::new(b.into())).unwrap_or_default())
        .unwrap()
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct ReqQuery<'a> {
    pub access_token: &'a str,
}
