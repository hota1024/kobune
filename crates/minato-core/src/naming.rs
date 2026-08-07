//! ブランチ名やプロジェクト名から DNS ラベルを導出する。
//!
//! ここで生成した名前は URL に現れ、いったん発行したら変えられない。
//! そのためサニタイズ結果は状態ストアに永続化し、規則を変更しても
//! 既存 workspace の URL が変わらないようにする（`state` モジュールを参照）。

use sha2::{Digest, Sha256};

/// DNS ラベル 1 つあたりの上限（RFC 1035）。
pub const MAX_LABEL_LEN: usize = 63;

/// 上限を超えたラベルを切り詰める長さ。残りをハッシュ接尾辞に使う。
const TRUNCATED_STEM_LEN: usize = 55;

/// ハッシュ接尾辞の長さ。
const HASH_SUFFIX_LEN: usize = 7;

/// サニタイズ後に何も残らなかった場合のフォールバック。
const FALLBACK: &str = "unnamed";

/// 任意の文字列を DNS ラベルとして使える形に正規化する。
///
/// - 小文字化し、`[a-z0-9-]` 以外を `-` に置換する
/// - 連続する `-` を 1 つに畳み、先頭末尾の `-` を除去する
/// - [`MAX_LABEL_LEN`] を超える場合は切り詰め、元の文字列のハッシュを付与する
///
/// 区切り記号（`/` `_` `-` `.` 空白）以外の文字が落ちた場合は、
/// 情報が消えたことを示すハッシュを付ける。
///
/// ```
/// # use minato_core::naming::sanitize_label;
/// assert_eq!(sanitize_label("feature/user-auth"), "feature-user-auth");
///
/// // 日本語が落ちた分はハッシュで補い、別のブランチと区別できるようにする。
/// let label = sanitize_label("feature/デモ環境");
/// assert!(label.starts_with("feature-"));
/// assert_ne!(label, sanitize_label("feature/検証環境"));
/// ```
pub fn sanitize_label(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_dash = true; // 先頭の `-` を落とすため true から始める
    // 情報が失われたかどうか。日本語のブランチ名は現実に使われるので、
    // 落ちた分をハッシュで補わないと `feature/デモ` と `feature/検証` が
    // どちらも `feature` になってしまう。
    let mut lost_information = false;

    for ch in input.chars() {
        let mapped = match ch {
            'a'..='z' | '0'..='9' => ch,
            'A'..='Z' => ch.to_ascii_lowercase(),
            // 区切りとして使われる記号は情報とみなさない。
            '/' | '_' | '-' | '.' | ' ' => '-',
            _ => {
                lost_information = true;
                '-'
            }
        };

        if mapped == '-' {
            if !prev_dash {
                out.push('-');
                prev_dash = true;
            }
        } else {
            out.push(mapped);
            prev_dash = false;
        }
    }

    while out.ends_with('-') {
        out.pop();
    }

    if out.is_empty() {
        return format!("{FALLBACK}-{}", short_hash(input));
    }

    if lost_information {
        // 元のブランチ名から決まるので、同じ入力なら常に同じラベルになる。
        return truncate_with_hash(&out, input);
    }

    if out.len() > MAX_LABEL_LEN {
        return truncate_with_hash(&out, input);
    }

    out
}

/// ラベルを上限に収め、元の入力のハッシュを付ける。
fn truncate_with_hash(label: &str, seed: &str) -> String {
    let mut stem = if label.len() > TRUNCATED_STEM_LEN {
        label[..TRUNCATED_STEM_LEN].to_string()
    } else {
        label.to_string()
    };

    // 切り詰めた結果が `-` で終わらないようにしてからハッシュを付ける。
    while stem.ends_with('-') {
        stem.pop();
    }

    if stem.is_empty() {
        return format!("{FALLBACK}-{}", short_hash(seed));
    }

    format!("{stem}-{}", short_hash(seed))
}

/// サニタイズ後のラベルが既存のものと衝突したときに、区別可能な別名を作る。
///
/// `seed` には元のブランチ名など、衝突している対象を一意に識別できるものを渡す。
pub fn disambiguate(label: &str, seed: &str) -> String {
    let suffix = short_hash(seed);
    let budget = MAX_LABEL_LEN - HASH_SUFFIX_LEN - 1;

    let mut stem = if label.len() > budget {
        label[..budget].to_string()
    } else {
        label.to_string()
    };
    while stem.ends_with('-') {
        stem.pop();
    }

    format!("{stem}-{suffix}")
}

/// すでに正規化済みのラベルかどうか。
pub fn is_valid_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= MAX_LABEL_LEN
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !label.contains("--")
}

/// サービスに割り当てるホスト名を組み立てる。
///
/// main worktree（`workspace` が `None`）では workspace ラベルを省略し、
/// `{service}.{project}.{suffix}` になる。
pub fn service_host(service: &str, workspace: Option<&str>, project: &str, suffix: &str) -> String {
    match workspace {
        Some(ws) => format!("{service}.{ws}.{project}.{suffix}"),
        None => format!("{service}.{project}.{suffix}"),
    }
}

/// ドメインを直接指定してサービスのホスト名を組み立てる。
///
/// `[project] domain` を設定した場合はプロジェクト名が接尾辞に現れないため、
/// [`service_host`] ではなくこちらを使う。
pub fn service_host_in(service: &str, workspace: Option<&str>, domain: &str) -> String {
    match workspace {
        Some(ws) => format!("{service}.{ws}.{domain}"),
        None => format!("{service}.{domain}"),
    }
}

/// Cloudflare Tunnel 向けのホスト名。
///
/// Tunnel 側ではサブドメインの階層を増やすとワイルドカード証明書が効かないため、
/// service と workspace を `-` で連結して 1 ラベルに収める。
pub fn tunnel_host(service: &str, workspace: Option<&str>, project: &str, domain: &str) -> String {
    match workspace {
        Some(ws) => format!("{service}-{ws}.{project}.{domain}"),
        None => format!("{service}.{project}.{domain}"),
    }
}

fn short_hash(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    let hex = format!("{digest:x}");
    hex[..HASH_SUFFIX_LEN].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_disallowed_characters() {
        assert_eq!(sanitize_label("feature/user-auth"), "feature-user-auth");
        assert_eq!(sanitize_label("release/v1.2.3"), "release-v1-2-3");

        // `#` は区切り記号ではないので、落ちたことをハッシュで示す。
        let hashed = sanitize_label("FIX_Bug#123");
        assert!(hashed.starts_with("fix-bug-123-"), "got: {hashed}");
    }

    #[test]
    fn collapses_and_trims_dashes() {
        assert_eq!(sanitize_label("--a//b--"), "a-b");
        assert_eq!(sanitize_label("a___b"), "a-b");
    }

    #[test]
    fn keeps_japanese_branches_distinguishable() {
        // 日本語のブランチ名は現実に使われる。落ちた情報をハッシュで
        // 補わないと、どれも同じラベルになって URL が衝突する。
        let a = sanitize_label("feature/デモ環境");
        let b = sanitize_label("feature/検証環境");

        assert_ne!(a, b, "別のブランチが同じラベルになってはいけない");
        assert!(a.starts_with("feature-"), "読める部分は残す: {a}");
        assert!(is_valid_label(&a), "{a}");
        assert!(is_valid_label(&b), "{b}");

        // 決定的であること。URL は一度発行したら変わらない。
        assert_eq!(a, sanitize_label("feature/デモ環境"));
    }

    #[test]
    fn ascii_only_branches_stay_clean() {
        // 記号だけの変換ではハッシュを付けない。読みやすさを損なう。
        assert_eq!(sanitize_label("feature/user-auth"), "feature-user-auth");
        assert_eq!(sanitize_label("fix_bug.123"), "fix-bug-123");
        assert_eq!(sanitize_label("release/v1.2.3"), "release-v1-2-3");
    }

    #[test]
    fn falls_back_when_nothing_remains() {
        let out = sanitize_label("///");
        assert!(out.starts_with("unnamed-"));
        assert!(is_valid_label(&out));
    }

    #[test]
    fn truncates_long_input_with_stable_hash() {
        let long = "feature/".to_string() + &"x".repeat(120);
        let a = sanitize_label(&long);
        let b = sanitize_label(&long);

        assert_eq!(a, b, "同じ入力からは同じラベルが出る必要がある");
        assert!(a.len() <= MAX_LABEL_LEN);
        assert!(is_valid_label(&a));
    }

    #[test]
    fn different_long_inputs_do_not_collide() {
        let a = sanitize_label(&("feature/".to_string() + &"x".repeat(120)));
        let b = sanitize_label(&("feature/".to_string() + &"x".repeat(121)));
        assert_ne!(a, b);
    }

    #[test]
    fn sanitized_output_is_idempotent() {
        // 出力は ASCII のみなので、二度通しても増えない。
        for input in ["feature/user-auth", "release/v1.2.3", "a___b"] {
            let once = sanitize_label(input);
            assert_eq!(sanitize_label(&once), once, "input = {input}");
        }
    }

    #[test]
    fn disambiguate_stays_within_limits() {
        let long = "a".repeat(MAX_LABEL_LEN);
        let out = disambiguate(&long, "seed");
        assert!(out.len() <= MAX_LABEL_LEN);
        assert!(is_valid_label(&out));
    }

    #[test]
    fn builds_hostnames_from_a_domain() {
        assert_eq!(
            service_host_in("web", Some("feat-1"), "myapp.localhost"),
            "web.feat-1.myapp.localhost"
        );
        assert_eq!(
            service_host_in("web", None, "myapp.localhost"),
            "web.myapp.localhost"
        );
        // domain を明示した場合はプロジェクト名が入らない。
        assert_eq!(
            service_host_in("api", Some("feat-1"), "dev.example.com"),
            "api.feat-1.dev.example.com"
        );
    }

    #[test]
    fn builds_hostnames() {
        assert_eq!(
            service_host("web", Some("feat-1"), "myapp", "localhost"),
            "web.feat-1.myapp.localhost"
        );
        assert_eq!(
            service_host("web", None, "myapp", "localhost"),
            "web.myapp.localhost"
        );
        assert_eq!(
            tunnel_host("web", Some("feat-1"), "myapp", "example.com"),
            "web-feat-1.myapp.example.com"
        );
    }
}
