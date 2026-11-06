/// 检查 baseUrl 的路径后缀；余额 URL 模板据此选择 origin，避免重复拼入 /anthropic 等路径。
pub fn base_url_has_path_suffix(base_url: &str) -> bool {
    let trimmed = base_url.trim();
    if trimmed.is_empty() { return false; }
    if let Ok(url) = url::Url::parse(trimmed) {
        let path = url.path();
        return !path.is_empty() && path != "/";
    }
    if let Some(after_scheme) = trimmed.split("://").nth(1) {
        if let Some(first_slash) = after_scheme.find('/') {
            return !after_scheme[first_slash + 1..].is_empty();
        }
    }
    false
}
