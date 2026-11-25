//! Artifact 的 URI `zcode-artifact://<sessionId>/<artifactId>`（docs/specs/rust-m11-node-storage.md §2.1）：写进 tool part 与
//! attachment 的引用。文件本身（目录、文件名、读写）在 node-store。写法只有 kernel-api 的 `artifact_uri` 一份：前端给出的上传附件
//! 引用与 file part 的 url 必须是同一个字符串（docs/specs/rust-cli/node-store.md §4.4.1）。

use rust_cli_kernel_api::store::ArtifactRef;

/// Artifact 的 URI（写进 part 时使用）。
pub fn uri(artifact: &ArtifactRef) -> String {
    rust_cli_kernel_api::artifact_uri::uri(&artifact.session, &artifact.id)
}
