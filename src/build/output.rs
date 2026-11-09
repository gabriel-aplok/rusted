pub(crate) fn escape(value: &str) -> String {
    crate::util::json::escape(value)
}

pub(crate) fn compiler_message(package_id: &str, message: &str) -> String {
    format!(
        "{{\"reason\":\"compiler-message\",\"package_id\":\"{}\",\"message\":{}}}",
        escape(package_id),
        message
    )
}

pub(crate) fn compiler_artifact(
    package_id: &str,
    target_name: &str,
    kind: &str,
    filename: &str,
    fresh: bool,
) -> String {
    format!(
        "{{\"reason\":\"compiler-artifact\",\"package_id\":\"{}\",\"target\":{{\"name\":\"{}\",\"kind\":[\"{}\"]}},\"filenames\":[\"{}\"],\"fresh\":{}}}",
        escape(package_id),
        escape(target_name),
        kind,
        escape(filename),
        fresh
    )
}

pub(crate) fn build_finished(success: bool, warnings: usize) -> String {
    format!("{{\"reason\":\"build-finished\",\"success\":{success},\"warnings\":{warnings}}}")
}

pub(crate) fn build_stats(
    rustc_calls: usize,
    cache_hits: usize,
    bytes_written: u64,
    wall_ms: u128,
) -> String {
    format!(
        "{{\"reason\":\"build-stats\",\"rustc_calls\":{rustc_calls},\"cache_hits\":{cache_hits},\"bytes_written\":{bytes_written},\"wall_ms\":{wall_ms}}}"
    )
}

#[cfg(test)]
mod tests {
    use super::{build_finished, build_stats, compiler_artifact, compiler_message, escape};

    #[test]
    fn escapes_json_strings() {
        assert_eq!(escape("a\"b\\c\n"), "a\\\"b\\\\c\\n");
        assert_eq!(escape("\u{0001}"), "\\u0001");
    }

    #[test]
    fn compiler_message_is_stable() {
        let line = compiler_message("demo 0.1.0", r#"{"level":"warning"}"#);
        assert_eq!(
            line,
            r#"{"reason":"compiler-message","package_id":"demo 0.1.0","message":{"level":"warning"}}"#
        );
    }

    #[test]
    fn compiler_artifact_is_stable() {
        assert_eq!(
            compiler_artifact("demo 0.1.0", "demo", "bin", "/tmp/demo", false),
            r#"{"reason":"compiler-artifact","package_id":"demo 0.1.0","target":{"name":"demo","kind":["bin"]},"filenames":["/tmp/demo"],"fresh":false}"#
        );
    }

    #[test]
    fn build_finished_is_stable() {
        assert_eq!(
            build_finished(true, 2),
            r#"{"reason":"build-finished","success":true,"warnings":2}"#
        );
    }

    #[test]
    fn build_stats_is_stable() {
        assert_eq!(
            build_stats(3, 1, 4096, 12),
            r#"{"reason":"build-stats","rustc_calls":3,"cache_hits":1,"bytes_written":4096,"wall_ms":12}"#
        );
    }
}
