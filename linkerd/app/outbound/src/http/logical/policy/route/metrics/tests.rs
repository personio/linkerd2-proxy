use super::{
    super::{Grpc, Http, Route},
    labels,
    test_util::*,
    LabelGrpcRouteRsp, LabelHttpRouteRsp, RequestMetrics,
};
use bytes::{Buf, Bytes};
use http_body::Body;
use http_body_util::BodyExt;
use linkerd_app_core::{
    dns,
    svc::{
        self,
        http::{uri::Uri, BoxBody},
        Layer, NewService,
    },
};
use linkerd_http_prom::body_data::request::RequestBodyFamilies;
use linkerd_proxy_client_policy as policy;
use std::task::Poll;

static GRPC_STATUS: http::HeaderName = http::HeaderName::from_static("grpc-status");
static GRPC_STATUS_OK: http::HeaderValue = http::HeaderValue::from_static("0");

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn http_request_statuses() {
    const EXPORT_HOSTNAME_LABELS: bool = true;
    let _trace = linkerd_tracing::test::trace_init();

    let super::HttpRouteMetrics {
        requests,
        body_data,
        ..
    } = super::HttpRouteMetrics::default();
    let parent_ref = crate::ParentRef(policy::Meta::new_default("parent"));
    let route_ref = crate::RouteRef(policy::Meta::new_default("route"));
    let (mut svc, mut handle) = mock_http_route_metrics(
        &requests,
        &body_data,
        &parent_ref,
        &route_ref,
        EXPORT_HOSTNAME_LABELS,
    );

    // Send one request and ensure it's counted.
    let ok = requests.get_statuses(&labels::Rsp(
        labels::Route::new(parent_ref.clone(), route_ref.clone(), None),
        labels::HttpRsp {
            status: Some(http::StatusCode::OK),
            error: None,
        },
    ));
    send_assert_incremented(&ok, &mut handle, &mut svc, Default::default(), |tx| {
        tx.send_response(
            http::Response::builder()
                .status(200)
                .body(BoxBody::default())
                .unwrap(),
        )
    })
    .await;

    // Send another request and ensure it's counted with a different response
    // status.
    let no_content = requests.get_statuses(&labels::Rsp(
        labels::Route::new(parent_ref.clone(), route_ref.clone(), None),
        labels::HttpRsp {
            status: Some(http::StatusCode::NO_CONTENT),
            error: None,
        },
    ));
    send_assert_incremented(
        &no_content,
        &mut handle,
        &mut svc,
        Default::default(),
        |tx| {
            tx.send_response(
                http::Response::builder()
                    .status(204)
                    .body(BoxBody::default())
                    .unwrap(),
            )
        },
    )
    .await;

    // Emit a response with an error and ensure it's counted.
    let unknown = requests.get_statuses(&labels::Rsp(
        labels::Route::new(parent_ref.clone(), route_ref.clone(), None),
        labels::HttpRsp {
            status: None,
            error: Some(labels::Error::Unknown),
        },
    ));
    send_assert_incremented(&unknown, &mut handle, &mut svc, Default::default(), |tx| {
        tx.send_error("a spooky ghost")
    })
    .await;

    // Emit a successful response with a body that fails and ensure that both
    // the status and error are recorded.
    let mixed = requests.get_statuses(&labels::Rsp(
        labels::Route::new(parent_ref, route_ref, None),
        labels::HttpRsp {
            status: Some(http::StatusCode::OK),
            error: Some(labels::Error::Unknown),
        },
    ));
    send_assert_incremented(&mixed, &mut handle, &mut svc, Default::default(), |tx| {
        tx.send_response(
            http::Response::builder()
                .status(200)
                .body(BoxBody::new(MockBody::error("a spooky ghost")))
                .unwrap(),
        )
    })
    .await;

    assert_eq!(unknown.get(), 1);
    assert_eq!(ok.get(), 1);
    assert_eq!(no_content.get(), 1);
    assert_eq!(mixed.get(), 1);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn http_request_hostnames() {
    const EXPORT_HOSTNAME_LABELS: bool = true;
    const HOST_1: &str = "great.website";
    const URI_1_1: &str = "https://great.website/path/to/index.html#fragment";
    const URI_1_2: &str = "https://great.website/another/index.html";
    const HOST_2: &str = "different.website";
    const URI_2: &str = "https://different.website/index.html";
    const URI_3: &str = "https://[3fff::]/index.html";

    let _trace = linkerd_tracing::test::trace_init();

    let super::HttpRouteMetrics {
        requests,
        body_data,
        ..
    } = super::HttpRouteMetrics::default();
    let parent_ref = crate::ParentRef(policy::Meta::new_default("parent"));
    let route_ref = crate::RouteRef(policy::Meta::new_default("route"));
    let (mut svc, mut handle) = mock_http_route_metrics(
        &requests,
        &body_data,
        &parent_ref,
        &route_ref,
        EXPORT_HOSTNAME_LABELS,
    );

    let get_counter = |host: Option<&'static str>, status: Option<http::StatusCode>| {
        requests.get_statuses(&labels::Rsp(
            labels::Route::new_with_name(
                parent_ref.clone(),
                route_ref.clone(),
                host.map(str::parse::<dns::Name>).map(Result::unwrap),
            ),
            labels::HttpRsp {
                status,
                error: None,
            },
        ))
    };

    let host1_ok = get_counter(Some(HOST_1), Some(http::StatusCode::OK));
    let host1_teapot = get_counter(Some(HOST_1), Some(http::StatusCode::IM_A_TEAPOT));
    let host2_ok = get_counter(Some(HOST_2), Some(http::StatusCode::OK));
    let unlabeled_ok = get_counter(None, Some(http::StatusCode::OK));

    // Send one request and ensure it's counted.
    send_assert_incremented(
        &host1_ok,
        &mut handle,
        &mut svc,
        http::Request::builder()
            .uri(URI_1_1)
            .body(BoxBody::default())
            .unwrap(),
        |tx| {
            tx.send_response(
                http::Response::builder()
                    .status(200)
                    .body(BoxBody::default())
                    .unwrap(),
            )
        },
    )
    .await;
    assert_eq!(host1_ok.get(), 1);
    assert_eq!(host1_teapot.get(), 0);
    assert_eq!(host2_ok.get(), 0);

    // Send another request to a different path on the same host.
    send_assert_incremented(
        &host1_teapot,
        &mut handle,
        &mut svc,
        http::Request::builder()
            .uri(URI_1_2)
            .body(BoxBody::default())
            .unwrap(),
        |tx| {
            tx.send_response(
                http::Response::builder()
                    .status(418)
                    .body(BoxBody::default())
                    .unwrap(),
            )
        },
    )
    .await;
    assert_eq!(host1_ok.get(), 1);
    assert_eq!(host1_teapot.get(), 1);
    assert_eq!(host2_ok.get(), 0);

    // Send a request to a different host.
    send_assert_incremented(
        &host2_ok,
        &mut handle,
        &mut svc,
        http::Request::builder()
            .uri(URI_2)
            .body(BoxBody::default())
            .unwrap(),
        |tx| {
            tx.send_response(
                http::Response::builder()
                    .status(200)
                    .body(BoxBody::default())
                    .unwrap(),
            )
        },
    )
    .await;
    assert_eq!(host1_ok.get(), 1);
    assert_eq!(host1_teapot.get(), 1);
    assert_eq!(host2_ok.get(), 1);

    // Send a request to a url with an ip address host component, show that it is not labeled.
    send_assert_incremented(
        &unlabeled_ok,
        &mut handle,
        &mut svc,
        http::Request::builder()
            .uri(URI_3)
            .body(BoxBody::default())
            .unwrap(),
        |tx| {
            tx.send_response(
                http::Response::builder()
                    .status(200)
                    .body(BoxBody::default())
                    .unwrap(),
            )
        },
    )
    .await;
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn http_request_hostnames_disabled() {
    const EXPORT_HOSTNAME_LABELS: bool = false;
    const HOST_1: &str = "great.website";
    const URI_1_1: &str = "https://great.website/path/to/index.html#fragment";
    const HOST_2: &str = "different.website";
    const URI_2: &str = "https://different.website/index.html";
    const URI_3: &str = "https://[3fff::]/index.html";

    let _trace = linkerd_tracing::test::trace_init();

    let super::HttpRouteMetrics {
        requests,
        body_data,
        ..
    } = super::HttpRouteMetrics::default();
    let parent_ref = crate::ParentRef(policy::Meta::new_default("parent"));
    let route_ref = crate::RouteRef(policy::Meta::new_default("route"));
    let (mut svc, mut handle) = mock_http_route_metrics(
        &requests,
        &body_data,
        &parent_ref,
        &route_ref,
        EXPORT_HOSTNAME_LABELS,
    );

    let get_counter = |host: Option<&'static str>, status: Option<http::StatusCode>| {
        requests.get_statuses(&labels::Rsp(
            labels::Route::new_with_name(
                parent_ref.clone(),
                route_ref.clone(),
                host.map(str::parse::<dns::Name>).map(Result::unwrap),
            ),
            labels::HttpRsp {
                status,
                error: None,
            },
        ))
    };

    let host1_ok = get_counter(Some(HOST_1), Some(http::StatusCode::OK));
    let host2_ok = get_counter(Some(HOST_2), Some(http::StatusCode::OK));
    let unlabeled_ok = get_counter(None, Some(http::StatusCode::OK));

    // Send one request and ensure it's counted.
    send_assert_incremented(
        &unlabeled_ok,
        &mut handle,
        &mut svc,
        http::Request::builder()
            .uri(URI_1_1)
            .body(BoxBody::default())
            .unwrap(),
        |tx| {
            tx.send_response(
                http::Response::builder()
                    .status(200)
                    .body(BoxBody::default())
                    .unwrap(),
            )
        },
    )
    .await;
    assert_eq!(host1_ok.get(), 0);
    assert_eq!(host2_ok.get(), 0);
    assert_eq!(unlabeled_ok.get(), 1);

    // Send a request to a different host.
    send_assert_incremented(
        &unlabeled_ok,
        &mut handle,
        &mut svc,
        http::Request::builder()
            .uri(URI_2)
            .body(BoxBody::default())
            .unwrap(),
        |tx| {
            tx.send_response(
                http::Response::builder()
                    .status(200)
                    .body(BoxBody::default())
                    .unwrap(),
            )
        },
    )
    .await;
    assert_eq!(host1_ok.get(), 0);
    assert_eq!(host2_ok.get(), 0);
    assert_eq!(unlabeled_ok.get(), 2);

    // Send a request to a url with an ip address host component, show that it is not labeled.
    send_assert_incremented(
        &unlabeled_ok,
        &mut handle,
        &mut svc,
        http::Request::builder()
            .uri(URI_3)
            .body(BoxBody::default())
            .unwrap(),
        |tx| {
            tx.send_response(
                http::Response::builder()
                    .status(200)
                    .body(BoxBody::default())
                    .unwrap(),
            )
        },
    )
    .await;
    assert_eq!(unlabeled_ok.get(), 3);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn http_route_request_body_frames() {
    use linkerd_http_prom::body_data::request::BodyDataMetrics;

    const EXPORT_HOSTNAME_LABELS: bool = true;
    let _trace = linkerd_tracing::test::trace_init();

    let super::HttpRouteMetrics {
        requests,
        body_data,
        ..
    } = super::HttpRouteMetrics::default();
    let parent_ref = crate::ParentRef(policy::Meta::new_default("parent"));
    let route_ref = crate::RouteRef(policy::Meta::new_default("route"));
    let (mut svc, mut handle) = mock_http_route_metrics(
        &requests,
        &body_data,
        &parent_ref,
        &route_ref,
        EXPORT_HOSTNAME_LABELS,
    );
    handle.allow(1);

    let labels = labels::Route::new(
        parent_ref,
        route_ref,
        Some(&Uri::from_static("http://frame.count.test/")),
    );
    let BodyDataMetrics {
        // TODO(kate): currently, histograms do not expose their observation count or sum. so,
        // we're left unable to exercise these metrics until prometheus/client_rust#242 lands.
        //   - https://github.com/prometheus/client_rust/pull/241
        //   - https://github.com/prometheus/client_rust/pull/242
        #[cfg(feature = "prometheus-client-rust-242")]
        frame_size,
        ..
    } = body_data.metrics(&labels);

    // Create a request whose body is backed by a channel that we can send chunks to.
    tracing::info!("creating request");
    let (req, tx) = {
        let (tx, body) =
            http_body_util::channel::Channel::<bytes::Bytes, std::convert::Infallible>::new(1024);
        let body = BoxBody::new(body);
        let req = http::Request::builder()
            .uri("http://frame.count.test")
            .method("BARK")
            .body(body)
            .unwrap();
        (req, tx)
    };

    // Before the service has been called, the counters should be zero.
    #[cfg(feature = "prometheus-client-rust-242")]
    {
        assert_eq!(frame_size.count(), 0);
        assert_eq!(frame_size.sum(), 0);
    }

    // Call the service.
    tracing::info!("sending request to service");
    let (fut, resp_tx, rx) = {
        use tower::{Service, ServiceExt};
        tracing::info!("calling service");
        let fut = svc.ready().await.expect("ready").call(req);
        let (req, send_resp) = handle.next_request().await.unwrap();
        let (parts, rx) = req.into_parts();
        debug_assert_eq!(parts.method.as_str(), "BARK");
        (fut, send_resp, rx)
    };

    // Before the client has sent any body chunks, the counters should be zero.
    #[cfg(feature = "prometheus-client-rust-242")]
    {
        assert_eq!(frame_size.count(), 0);
        assert_eq!(frame_size.sum(), 0);
    }

    // Send a response back to the client.
    tracing::info!("sending request to service");
    let resp = {
        use http::{Response, StatusCode};
        let body = BoxBody::from_static("earl grey");
        let resp = Response::builder()
            .status(StatusCode::IM_A_TEAPOT)
            .body(body)
            .unwrap();
        resp_tx.send_response(resp);
        fut.await.expect("resp")
    };

    // The counters should still be zero.
    #[cfg(feature = "prometheus-client-rust-242")]
    {
        assert_eq!(frame_size.count(), 0);
        assert_eq!(frame_size.sum(), 0);
    }

    // Read the response body.
    tracing::info!("reading response body");
    {
        use http_body_util::BodyExt;
        let (parts, body) = resp.into_parts();
        debug_assert_eq!(parts.status, 418);
        let bytes = body.collect().await.expect("resp body").to_bytes();
        debug_assert_eq!(bytes, "earl grey");
    }

    // Reading the response body should not affect the counters should still be zero.
    #[cfg(feature = "prometheus-client-rust-242")]
    {
        assert_eq!(frame_size.count(), 0);
        assert_eq!(frame_size.sum(), 0);
    }

    /// Returns the next chunk from a boxed body.
    async fn read_chunk(body: &mut std::pin::Pin<Box<BoxBody>>) -> Vec<u8> {
        use {
            bytes::Buf,
            http_body::Body,
            std::task::{Context, Poll},
        };
        let mut ctx = Context::from_waker(futures_util::task::noop_waker_ref());
        let frame = match body.as_mut().poll_frame(&mut ctx) {
            Poll::Ready(Some(Ok(f))) => f,
            _ => panic!("next chunk should be ready"),
        };
        frame.into_data().ok().expect("data frame").chunk().to_vec()
    }

    // And now, send request body bytes.
    tracing::info!("sending request body bytes");
    {
        // Get the client's sending half, and the server's receiving half of the request body.
        let (mut tx, mut rx) = (tx, Box::pin(rx));

        tx.send_data(b"milk".as_slice().into()).await.unwrap();
        let chunk = read_chunk(&mut rx).await;
        debug_assert_eq!(chunk, b"milk");
        #[cfg(feature = "prometheus-client-rust-242")]
        assert_eq!(frames_total.get(), 1); // bytes are counted once polled.
        #[cfg(feature = "prometheus-client-rust-242")]
        assert_eq!(frames_bytes.get(), 4);

        tx.send_data(b"syrup".as_slice().into()).await.unwrap();
        let chunk = read_chunk(&mut rx).await;
        debug_assert_eq!(chunk, b"syrup");
        #[cfg(feature = "prometheus-client-rust-242")]
        assert_eq!(frames_total.get(), 2);
        #[cfg(feature = "prometheus-client-rust-242")]
        assert_eq!(frames_bytes.get(), 4 + 5);
    }

    tracing::info!("passed");
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn http_response_body_drop_on_eos() {
    use linkerd_app_core::svc::{Service, ServiceExt};

    const EXPORT_HOSTNAME_LABELS: bool = false;
    let _trace = linkerd_tracing::test::trace_init();

    let super::HttpRouteMetrics {
        requests,
        body_data,
        ..
    } = super::HttpRouteMetrics::default();
    let parent_ref = crate::ParentRef(policy::Meta::new_default("parent"));
    let route_ref = crate::RouteRef(policy::Meta::new_default("route"));
    let (mut svc, mut handle) = mock_http_route_metrics(
        &requests,
        &body_data,
        &parent_ref,
        &route_ref,
        EXPORT_HOSTNAME_LABELS,
    );

    // Define a request and a response.
    let req = http::Request::default();
    let rsp = http::Response::builder()
        .status(200)
        .body(BoxBody::from_static("contents"))
        .unwrap();

    // Two counters for 200 responses that do/don't have an error.
    let ok = requests.get_statuses(&labels::Rsp(
        labels::Route::new(parent_ref.clone(), route_ref.clone(), None),
        labels::HttpRsp {
            status: Some(http::StatusCode::OK),
            error: None,
        },
    ));
    let err = requests.get_statuses(&labels::Rsp(
        labels::Route::new(parent_ref.clone(), route_ref.clone(), None),
        labels::HttpRsp {
            status: Some(http::StatusCode::OK),
            error: Some(labels::Error::Unknown),
        },
    ));
    debug_assert_eq!(ok.get(), 0);
    debug_assert_eq!(err.get(), 0);

    // Send the request, and obtain the response.
    let mut body = {
        handle.allow(1);
        svc.ready().await.expect("ready");
        let mut call = svc.call(req);
        let (_req, tx) = tokio::select! {
            _ = (&mut call) => unreachable!(),
            res = handle.next_request() => res.unwrap(),
        };
        assert_eq!(ok.get(), 0);
        tx.send_response(rsp);
        call.await.unwrap().into_body()
    };

    // The counters are not incremented yet.
    assert_eq!(ok.get(), 0);
    assert_eq!(err.get(), 0);

    // Poll a frame out of the body.
    let data = body
        .frame()
        .await
        .expect("yields a result")
        .expect("yields a frame")
        .into_data()
        .ok()
        .expect("yields data");
    assert_eq!(data.chunk(), "contents".as_bytes());
    assert_eq!(data.remaining(), "contents".len());

    // Show that the body reports itself as being complete.
    debug_assert!(body.is_end_stream());
    assert_eq!(ok.get(), 1);
    assert_eq!(err.get(), 0);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn http_response_body_drop_early() {
    use linkerd_app_core::svc::{Service, ServiceExt};

    const EXPORT_HOSTNAME_LABELS: bool = false;
    let _trace = linkerd_tracing::test::trace_init();

    let super::HttpRouteMetrics {
        requests,
        body_data,
        ..
    } = super::HttpRouteMetrics::default();
    let parent_ref = crate::ParentRef(policy::Meta::new_default("parent"));
    let route_ref = crate::RouteRef(policy::Meta::new_default("route"));
    let (mut svc, mut handle) = mock_http_route_metrics(
        &requests,
        &body_data,
        &parent_ref,
        &route_ref,
        EXPORT_HOSTNAME_LABELS,
    );

    // Define a request and a response.
    let req = http::Request::default();
    let rsp = http::Response::builder()
        .status(200)
        .body(BoxBody::from_static("contents"))
        .unwrap();

    // Two counters for 200 responses that do/don't have an error.
    let ok = requests.get_statuses(&labels::Rsp(
        labels::Route::new(parent_ref.clone(), route_ref.clone(), None),
        labels::HttpRsp {
            status: Some(http::StatusCode::OK),
            error: None,
        },
    ));
    let err = requests.get_statuses(&labels::Rsp(
        labels::Route::new(parent_ref.clone(), route_ref.clone(), None),
        labels::HttpRsp {
            status: Some(http::StatusCode::OK),
            error: Some(labels::Error::Unknown),
        },
    ));
    debug_assert_eq!(ok.get(), 0);
    debug_assert_eq!(err.get(), 0);

    // Send the request, and obtain the response.
    let body = {
        handle.allow(1);
        svc.ready().await.expect("ready");
        let mut call = svc.call(req);
        let (_req, tx) = tokio::select! {
            _ = (&mut call) => unreachable!(),
            res = handle.next_request() => res.unwrap(),
        };
        assert_eq!(ok.get(), 0);
        tx.send_response(rsp);
        call.await.unwrap().into_body()
    };

    // The counters are not incremented yet.
    assert_eq!(ok.get(), 0);
    assert_eq!(err.get(), 0);

    // The body reports an error if it was not completed.
    drop(body);
    assert_eq!(ok.get(), 0);
    assert_eq!(err.get(), 1);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn grpc_request_statuses_ok() {
    const EXPORT_HOSTNAME_LABELS: bool = true;
    let _trace = linkerd_tracing::test::trace_init();

    let super::GrpcRouteMetrics {
        requests,
        body_data,
        ..
    } = super::GrpcRouteMetrics::default();
    let parent_ref = crate::ParentRef(policy::Meta::new_default("parent"));
    let route_ref = crate::RouteRef(policy::Meta::new_default("route"));

    let (mut svc, mut handle) = mock_grpc_route_metrics(
        &requests,
        &body_data,
        &parent_ref,
        &route_ref,
        EXPORT_HOSTNAME_LABELS,
    );

    // Send one request and ensure it's counted.
    let ok = requests.get_statuses(&labels::Rsp(
        labels::Route::new(
            parent_ref.clone(),
            route_ref.clone(),
            Some(&Uri::from_static(MOCK_GRPC_REQ_URI)),
        ),
        labels::GrpcRsp {
            status: Some(tonic::Code::Ok),
            error: None,
        },
    ));
    send_assert_incremented(
        &ok,
        &mut handle,
        &mut svc,
        http::Request::builder()
            .method("POST")
            .uri("http://host/svc/method")
            .body(Default::default())
            .unwrap(),
        |tx| {
            tx.send_response(
                http::Response::builder()
                    .body(BoxBody::new(MockBody::grpc_status(0)))
                    .unwrap(),
            )
        },
    )
    .await;
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn grpc_request_statuses_not_found() {
    const EXPORT_HOSTNAME_LABELS: bool = true;
    let _trace = linkerd_tracing::test::trace_init();

    let super::GrpcRouteMetrics {
        requests,
        body_data,
        ..
    } = super::GrpcRouteMetrics::default();
    let parent_ref = crate::ParentRef(policy::Meta::new_default("parent"));
    let route_ref = crate::RouteRef(policy::Meta::new_default("route"));

    let (mut svc, mut handle) = mock_grpc_route_metrics(
        &requests,
        &body_data,
        &parent_ref,
        &route_ref,
        EXPORT_HOSTNAME_LABELS,
    );

    // Send another request and ensure it's counted with a different response
    // status.
    let not_found = requests.get_statuses(&labels::Rsp(
        labels::Route::new(
            parent_ref.clone(),
            route_ref.clone(),
            Some(&Uri::from_static(MOCK_GRPC_REQ_URI)),
        ),
        labels::GrpcRsp {
            status: Some(tonic::Code::NotFound),
            error: None,
        },
    ));
    send_assert_incremented(
        &not_found,
        &mut handle,
        &mut svc,
        http::Request::builder()
            .method("POST")
            .uri("http://host/svc/method")
            .body(Default::default())
            .unwrap(),
        |tx| {
            tx.send_response(
                http::Response::builder()
                    .body(BoxBody::new(MockBody::grpc_status(5)))
                    .unwrap(),
            )
        },
    )
    .await;
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn grpc_request_statuses_error_response() {
    const EXPORT_HOSTNAME_LABELS: bool = true;
    let _trace = linkerd_tracing::test::trace_init();

    let super::GrpcRouteMetrics {
        requests,
        body_data,
        ..
    } = super::GrpcRouteMetrics::default();
    let parent_ref = crate::ParentRef(policy::Meta::new_default("parent"));
    let route_ref = crate::RouteRef(policy::Meta::new_default("route"));
    let (mut svc, mut handle) = mock_grpc_route_metrics(
        &requests,
        &body_data,
        &parent_ref,
        &route_ref,
        EXPORT_HOSTNAME_LABELS,
    );

    let unknown = requests.get_statuses(&labels::Rsp(
        labels::Route::new(
            parent_ref.clone(),
            route_ref.clone(),
            Some(&Uri::from_static(MOCK_GRPC_REQ_URI)),
        ),
        labels::GrpcRsp {
            status: None,
            error: Some(labels::Error::Unknown),
        },
    ));
    send_assert_incremented(
        &unknown,
        &mut handle,
        &mut svc,
        http::Request::builder()
            .method("POST")
            .uri("http://host/svc/method")
            .body(Default::default())
            .unwrap(),
        |tx| tx.send_error("a spooky ghost"),
    )
    .await;
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn grpc_request_statuses_error_body() {
    const EXPORT_HOSTNAME_LABELS: bool = true;
    let _trace = linkerd_tracing::test::trace_init();

    let super::GrpcRouteMetrics {
        requests,
        body_data,
        ..
    } = super::GrpcRouteMetrics::default();
    let parent_ref = crate::ParentRef(policy::Meta::new_default("parent"));
    let route_ref = crate::RouteRef(policy::Meta::new_default("route"));
    let (mut svc, mut handle) = mock_grpc_route_metrics(
        &requests,
        &body_data,
        &parent_ref,
        &route_ref,
        EXPORT_HOSTNAME_LABELS,
    );

    let unknown = requests.get_statuses(&labels::Rsp(
        labels::Route::new(
            parent_ref.clone(),
            route_ref.clone(),
            Some(&Uri::from_static(MOCK_GRPC_REQ_URI)),
        ),
        labels::GrpcRsp {
            status: None,
            error: Some(labels::Error::Unknown),
        },
    ));
    send_assert_incremented(
        &unknown,
        &mut handle,
        &mut svc,
        http::Request::builder()
            .method("POST")
            .uri("http://host/svc/method")
            .body(Default::default())
            .unwrap(),
        |tx| {
            tx.send_response(
                http::Response::builder()
                    .body(BoxBody::new(MockBody::error("a spooky ghost")))
                    .unwrap(),
            )
        },
    )
    .await;
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn grpc_response_body_drop_on_eos() {
    use linkerd_app_core::svc::{Service, ServiceExt};

    const EXPORT_HOSTNAME_LABELS: bool = false;
    let _trace = linkerd_tracing::test::trace_init();

    let super::GrpcRouteMetrics {
        requests,
        body_data,
        ..
    } = super::GrpcRouteMetrics::default();
    let parent_ref = crate::ParentRef(policy::Meta::new_default("parent"));
    let route_ref = crate::RouteRef(policy::Meta::new_default("route"));
    let (mut svc, mut handle) = mock_grpc_route_metrics(
        &requests,
        &body_data,
        &parent_ref,
        &route_ref,
        EXPORT_HOSTNAME_LABELS,
    );

    // Define a request and a response.
    let req = http::Request::default();
    let rsp = http::Response::builder()
        .status(200)
        .body({
            let data = Poll::Ready(Some(Ok(Bytes::from_static(b"contents"))));
            let trailers = {
                let mut trailers = http::HeaderMap::with_capacity(1);
                trailers.insert(GRPC_STATUS.clone(), GRPC_STATUS_OK.clone());
                Poll::Ready(Some(Ok(trailers)))
            };
            let body = linkerd_mock_http_body::MockBody::default()
                .then_yield_data(data)
                .then_yield_trailer(trailers);
            BoxBody::new(body)
        })
        .unwrap();

    // Two counters for 200 responses that do/don't have an error.
    let ok = requests.get_statuses(&labels::Rsp(
        labels::Route::new(parent_ref.clone(), route_ref.clone(), None),
        labels::GrpcRsp {
            status: Some(tonic::Code::Ok),
            error: None,
        },
    ));
    let err = requests.get_statuses(&labels::Rsp(
        labels::Route::new(parent_ref.clone(), route_ref.clone(), None),
        labels::GrpcRsp {
            status: Some(tonic::Code::Ok),
            error: Some(labels::Error::Unknown),
        },
    ));
    debug_assert_eq!(ok.get(), 0);
    debug_assert_eq!(err.get(), 0);

    // Send the request, and obtain the response.
    let mut body = {
        handle.allow(1);
        svc.ready().await.expect("ready");
        let mut call = svc.call(req);
        let (_req, tx) = tokio::select! {
            _ = (&mut call) => unreachable!(),
            res = handle.next_request() => res.unwrap(),
        };
        assert_eq!(ok.get(), 0);
        tx.send_response(rsp);
        call.await.unwrap().into_body()
    };

    // The counters are not incremented yet.
    assert_eq!(ok.get(), 0);
    assert_eq!(err.get(), 0);

    // Poll a frame out of the body.
    let data = body
        .frame()
        .await
        .expect("yields a result")
        .expect("yields a frame")
        .into_data()
        .ok()
        .expect("yields data");
    assert_eq!(data.chunk(), "contents".as_bytes());
    assert_eq!(data.remaining(), "contents".len());

    // Poll the trailers out of the body.
    let trls = body
        .frame()
        .await
        .expect("yields a result")
        .expect("yields a frame")
        .into_trailers()
        .ok()
        .expect("yields trailers");
    assert_eq!(trls.get(&GRPC_STATUS).unwrap(), GRPC_STATUS_OK);

    // Show that the body reports itself as being complete.
    debug_assert!(body.is_end_stream());
    assert_eq!(ok.get(), 1);
    assert_eq!(err.get(), 0);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn grpc_response_body_drop_early() {
    use linkerd_app_core::svc::{Service, ServiceExt};

    const EXPORT_HOSTNAME_LABELS: bool = false;
    let _trace = linkerd_tracing::test::trace_init();

    let super::GrpcRouteMetrics {
        requests,
        body_data,
        ..
    } = super::GrpcRouteMetrics::default();
    let parent_ref = crate::ParentRef(policy::Meta::new_default("parent"));
    let route_ref = crate::RouteRef(policy::Meta::new_default("route"));
    let (mut svc, mut handle) = mock_grpc_route_metrics(
        &requests,
        &body_data,
        &parent_ref,
        &route_ref,
        EXPORT_HOSTNAME_LABELS,
    );

    // Define a request and a response.
    let req = http::Request::default();
    let rsp = http::Response::builder()
        .status(200)
        .body({
            let data = Poll::Ready(Some(Ok(Bytes::from_static(b"contents"))));
            let trailers = {
                let mut trailers = http::HeaderMap::with_capacity(1);
                trailers.insert(GRPC_STATUS.clone(), GRPC_STATUS_OK.clone());
                Poll::Ready(Some(Ok(trailers)))
            };
            let body = linkerd_mock_http_body::MockBody::default()
                .then_yield_data(data)
                .then_yield_trailer(trailers);
            BoxBody::new(body)
        })
        .unwrap();

    // Two counters for 200 responses that do/don't have an error.
    let ok = requests.get_statuses(&labels::Rsp(
        labels::Route::new(parent_ref.clone(), route_ref.clone(), None),
        labels::GrpcRsp {
            status: Some(tonic::Code::Ok),
            error: None,
        },
    ));
    let err = requests.get_statuses(&labels::Rsp(
        labels::Route::new(parent_ref.clone(), route_ref.clone(), None),
        labels::GrpcRsp {
            status: None,
            error: Some(labels::Error::Unknown),
        },
    ));
    debug_assert_eq!(ok.get(), 0);
    debug_assert_eq!(err.get(), 0);

    // Send the request, and obtain the response.
    let mut body = {
        handle.allow(1);
        svc.ready().await.expect("ready");
        let mut call = svc.call(req);
        let (_req, tx) = tokio::select! {
            _ = (&mut call) => unreachable!(),
            res = handle.next_request() => res.unwrap(),
        };
        assert_eq!(ok.get(), 0);
        tx.send_response(rsp);
        call.await.unwrap().into_body()
    };

    // The counters are not incremented yet.
    assert_eq!(ok.get(), 0);
    assert_eq!(err.get(), 0);

    // Poll a frame out of the body.
    let data = body
        .frame()
        .await
        .expect("yields a result")
        .expect("yields a frame")
        .into_data()
        .ok()
        .expect("yields data");
    assert_eq!(data.chunk(), "contents".as_bytes());
    assert_eq!(data.remaining(), "contents".len());

    // The counters are not incremented yet.
    debug_assert!(!body.is_end_stream());
    assert_eq!(ok.get(), 0);
    assert_eq!(err.get(), 0);

    // Then, drop the body without polling the trailers.
    drop(body);
    assert_eq!(ok.get(), 0);
    assert_eq!(err.get(), 1);
}

// === Utils ===

const MOCK_GRPC_REQ_URI: &str = "http://host/svc/method";

pub fn mock_http_route_metrics(
    metrics: &RequestMetrics<LabelHttpRouteRsp>,
    body_data: &RequestBodyFamilies<labels::Route>,
    parent_ref: &crate::ParentRef,
    route_ref: &crate::RouteRef,
    export_hostname_labels: bool,
) -> (svc::BoxHttp, Handle) {
    let req = http::Request::builder().body(()).unwrap();
    let (r#match, _) = policy::route::find(
        &[policy::http::Route {
            hosts: vec![],
            rules: vec![policy::route::Rule {
                matches: vec![policy::http::r#match::MatchRequest::default()],
                policy: policy::http::Policy {
                    meta: route_ref.0.clone(),
                    filters: [].into(),
                    distribution: policy::RouteDistribution::Empty,
                    params: policy::http::RouteParams {
                        export_hostname_labels,
                        ..Default::default()
                    },
                },
            }],
        }],
        &req,
    )
    .expect("find default route");

    let (tx, handle) = tower_test::mock::pair::<http::Request<BoxBody>, http::Response<BoxBody>>();
    let svc = super::layer(metrics, body_data)
        .layer(move |_t: Http<()>| tx.clone())
        .new_service(Http {
            r#match,
            params: Route {
                parent: (),
                addr: std::net::SocketAddr::new([0, 0, 0, 0].into(), 8080).into(),
                parent_ref: parent_ref.clone(),
                route_ref: route_ref.clone(),
                filters: [].into(),
                distribution: Default::default(),
                params: policy::http::RouteParams {
                    export_hostname_labels,
                    ..Default::default()
                },
            },
        });

    (svc::BoxHttp::new(svc), handle)
}

pub fn mock_grpc_route_metrics(
    metrics: &RequestMetrics<LabelGrpcRouteRsp>,
    body_data: &RequestBodyFamilies<labels::Route>,
    parent_ref: &crate::ParentRef,
    route_ref: &crate::RouteRef,
    export_hostname_labels: bool,
) -> (svc::BoxHttp, Handle) {
    let req = http::Request::builder()
        .method("POST")
        .uri(MOCK_GRPC_REQ_URI)
        .body(())
        .unwrap();
    let (r#match, _) = policy::route::find(
        &[policy::grpc::Route {
            hosts: vec![],
            rules: vec![policy::route::Rule {
                matches: vec![policy::grpc::r#match::MatchRoute::default()],
                policy: policy::grpc::Policy {
                    meta: route_ref.0.clone(),
                    filters: [].into(),
                    distribution: policy::RouteDistribution::Empty,
                    params: policy::grpc::RouteParams {
                        export_hostname_labels,
                        ..Default::default()
                    },
                },
            }],
        }],
        &req,
    )
    .expect("find default route");

    let (tx, handle) = tower_test::mock::pair::<http::Request<BoxBody>, http::Response<BoxBody>>();
    let svc = super::layer(metrics, body_data)
        .layer(move |_t: Grpc<()>| tx.clone())
        .new_service(Grpc {
            r#match,
            params: Route {
                parent: (),
                addr: std::net::SocketAddr::new([0, 0, 0, 0].into(), 8080).into(),
                parent_ref: parent_ref.clone(),
                route_ref: route_ref.clone(),
                filters: [].into(),
                distribution: Default::default(),
                params: policy::grpc::RouteParams {
                    export_hostname_labels,
                    ..Default::default()
                },
            },
        });

    (svc::BoxHttp::new(svc), handle)
}
