use axum::{
    body::Body,
    http::{StatusCode, header},
    response::Response,
};

const INDEX: &str = include_str!("../web/dist/index.html");

pub async fn index() -> Response {
    Response::builder()
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(Body::from(INDEX))
        .unwrap()
}

pub async fn asset(axum::extract::Path(path): axum::extract::Path<String>) -> Response {
    let (content_type, bytes): (&str, &[u8]) = match path.as_str() {
        "index-CY2wQssZ.css" => (
            "text/css; charset=utf-8",
            include_bytes!("../web/dist/assets/index-CY2wQssZ.css"),
        ),
        "index-DYnVaqeY.js" => (
            "application/javascript; charset=utf-8",
            include_bytes!("../web/dist/assets/index-DYnVaqeY.js"),
        ),
        _ => {
            return Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::empty())
                .unwrap();
        }
    };
    Response::builder()
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from(bytes))
        .unwrap()
}
