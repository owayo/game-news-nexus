//! 埋め込んだ IPAdic 辞書 (`dict/ipadic.hsd`。`build.rs` が大きさと SHA-256 を照合済み)。

use std::sync::{Arc, OnceLock};

use hasami::{Analyzer, Dictionary};

static IPADIC: &[u8] =
    hasami::include_hsd!(concat!(env!("CARGO_MANIFEST_DIR"), "/dict/ipadic.hsd"));

/// 解析器を作る。辞書はプロセスで 1 度だけ読み、解析器どうしで共有する (複製しない)。
pub(crate) fn analyzer() -> Analyzer {
    static SHARED: OnceLock<Arc<Dictionary>> = OnceLock::new();
    let dictionary = SHARED.get_or_init(|| {
        Arc::new(Dictionary::from_static(IPADIC).expect("埋め込んだ辞書は build.rs で照合済み"))
    });
    Analyzer::from_shared(Arc::clone(dictionary))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_dictionary_tokenizes() {
        let mut analyzer = analyzer();
        let tokens = analyzer.tokenize("定期メンテナンスを実施いたします。");
        let surfaces: Vec<&str> = tokens.iter().map(|token| &*token.surface).collect();
        assert_eq!(
            surfaces,
            ["定期", "メンテナンス", "を", "実施", "いたし", "ます", "。"]
        );
    }
}
