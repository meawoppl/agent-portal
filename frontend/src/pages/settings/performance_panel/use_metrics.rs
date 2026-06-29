//! Metrics fetch hook for the Performance settings panel.

use shared::api::{MetricBucket, MetricBucketsResponse};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use super::{bucket_param, TimeWindow};
use crate::utils::{self, FetchError, On401};

pub(super) struct PerformanceMetrics {
    pub buckets: Vec<MetricBucket>,
    pub loading: bool,
    pub error_msg: Option<String>,
}

/// Fetch per-turn metric buckets whenever the selected time window changes.
#[hook]
pub(super) fn use_performance_metrics(window: TimeWindow) -> PerformanceMetrics {
    let buckets = use_state(Vec::<MetricBucket>::new);
    let loading = use_state(|| true);
    let error_msg = use_state(|| None::<String>);

    {
        let buckets = buckets.clone();
        let loading = loading.clone();
        let error_msg = error_msg.clone();
        use_effect_with(window, move |&window| {
            loading.set(true);
            error_msg.set(None);
            let buckets = buckets.clone();
            let loading = loading.clone();
            let error_msg = error_msg.clone();
            spawn_local(async move {
                let path = format!(
                    "/api/metrics/turns?bucket={}&window={}",
                    bucket_param(window),
                    window.label()
                );
                match utils::fetch_json::<MetricBucketsResponse>(&path, On401::Ignore).await {
                    Ok(data) => {
                        buckets.set(data.buckets);
                    }
                    Err(FetchError::Decode(e)) => {
                        error_msg.set(Some(format!("Failed to parse response: {e}")));
                    }
                    Err(FetchError::Status(code)) => {
                        error_msg.set(Some(format!("Request failed: HTTP {code}")));
                    }
                    Err(FetchError::Network(e)) => {
                        error_msg.set(Some(format!("Network error: {e}")));
                    }
                }
                loading.set(false);
            });
            || ()
        });
    }

    PerformanceMetrics {
        buckets: (*buckets).clone(),
        loading: *loading,
        error_msg: (*error_msg).clone(),
    }
}
