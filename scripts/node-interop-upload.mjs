// rust-cli 与 Node CLI 的会话互操作检查的第三十段：上传的图片（docs/specs/rust-cli/node-store.md §4.4.1、§4.4.2，protocol-v5.md §39、§41，
// ATTACHMENTS、ATTACH-2）。
//
// rust-cli 写一个会话（node_interop `upload-write`：上传服务写法的 Artifact——data URL 文字；一条正文之后带本机附上的图片、上传的 PDF、
// 原样的上传图片与开回合时缩放之后另写的 JPEG 的用户输入，与它的输入记录；块带 origin，SMALL-4）：
// 1. Node 的 Artifact 存储按 URI 找到 rust-cli 写的文件，读回同一个 data URL；
// 2. 用户消息的 file part 是 Node 上传媒体的写法（url 是 URI、`storageKind: artifact`、`artifactUri`），PDF 的 mime 是 `application/pdf`；
//    Node 的冷恢复读出这两张图（history hydrator 经 Artifact 读 data URL），PDF 的 part 还原成 file 块；v4 的冷读合成给出 ref 是这些
//    URI 的附件；每条消息过 App 的 strict schema；
//    本机附上的 file part 带 Node 的 `filename`、`source`（界面给的路径）、`metadata.originalUrl`，Node 冷恢复的块 `source.placeholder`
//    是这个路径（模型不收图片时占位的名字）；上传的 PDF 带 `filename`，上传的图片没有（Node 相同）；
// 3. rust-cli 冷读取还原成同样的 Artifact 引用与 origin，输入记录带着上传附件。

import { appSchemaRejections } from "./node-interop-history.mjs";
import { importNode } from "./node-root.mjs";

export async function checkUpload(ctx) {
  const { nodeRoot, store, artifactStore, dir, rust, nodeHistory, expectEqual } = ctx;
  const { synthesizeEventsFromMessages } = await importNode(
    nodeRoot,
    "bootstrap/src/zcode-protocol-v4/transcript-hydration.ts",
  );
  const { filePartToContentBlock } = await importNode(
    nodeRoot,
    "core/src/agent/file-part-hydration.ts",
  );
  const written = rust("upload-write", dir);
  const sessionID = written.session;

  // 1. Node 找得到 rust-cli 写的 Artifact，读回同一个 data URL。
  const artifact = await artifactStore.readToolResultArtifact({ uri: written.uri });
  expectEqual(
    "Node reads the uploaded artifact as the same data URL",
    artifact.content,
    written.dataUrl,
  );

  // 2. Node 的写法与读法。
  const messages = await store.messages({ sessionID });
  const ask = messages.find((message) => message.info.id === written.ask);
  const files = ask?.parts.filter((part) => part.type === "file") ?? [];
  const artifactPart = (mime, uri) => ({
    mime,
    url: uri,
    storageKind: "artifact",
    artifactUri: uri,
  });
  expectEqual(
    "the uploaded media are Node's artifact file parts",
    files.map((file) => ({
      mime: file.mime,
      url: file.url,
      storageKind: file.metadata?.storageKind,
      artifactUri: file.metadata?.artifactUri,
    })),
    [
      artifactPart("image/png", written.localUri),
      artifactPart("application/pdf", written.pdfUri),
      artifactPart("image/png", written.uri),
      artifactPart("image/jpeg", written.jpegUri),
    ],
  );
  expectEqual(
    "the attached media keep their file like Node's file parts",
    files.map((file) => ({
      filename: file.filename,
      path: file.source?.path,
      text: file.source?.text,
      originalUrl: file.metadata?.originalUrl,
    })),
    [
      {
        filename: "pic.png",
        path: written.localPath,
        text: { value: written.localPath, start: 0, end: written.localPath.length },
        originalUrl: written.localPath,
      },
      { filename: "doc.pdf", path: undefined, text: undefined, originalUrl: undefined },
      { filename: undefined, path: undefined, text: undefined, originalUrl: undefined },
      { filename: undefined, path: undefined, text: undefined, originalUrl: undefined },
    ],
  );
  const local = await filePartToContentBlock(files[0], artifactStore);
  expectEqual(
    "Node's cold resume names the local image by its path",
    { type: local.type, dataUrl: local.dataUrl, placeholder: local.source?.placeholder },
    { type: "image", dataUrl: written.localDataUrl, placeholder: written.localPath },
  );
  const session = await store.getSession(sessionID);
  const history = await nodeHistory(store, artifactStore, session, sessionID);
  expectEqual("Node's cold history sees the images after the text", history[0], {
    role: "user",
    text: "What is in the shot?",
    images: [
      { type: "image", mediaType: "image/png", dataUrl: written.localDataUrl },
      { type: "image", mediaType: "image/png", dataUrl: written.dataUrl },
      { type: "image", mediaType: "image/jpeg", dataUrl: written.jpegDataUrl },
    ],
  });
  const pdf = await filePartToContentBlock(files[1], artifactStore);
  expectEqual(
    "Node's cold resume reads the PDF part as a file block",
    { type: pdf.type, mediaType: pdf.mediaType, dataUrl: pdf.dataUrl },
    { type: "file", mediaType: "application/pdf", dataUrl: written.pdfDataUrl },
  );
  const started = synthesizeEventsFromMessages(messages, { sessionId: sessionID }).find(
    (event) => event.type === "turn_started",
  );
  expectEqual(
    "the v4 cold synthesis shows the attachment with its URI",
    started?.payload.attachments?.map(({ mime, ref }) => ({ mime, ref })),
    [
      { mime: "image/png", ref: written.localUri },
      { mime: "application/pdf", ref: written.pdfUri },
      { mime: "image/png", ref: written.uri },
      { mime: "image/jpeg", ref: written.jpegUri },
    ],
  );
  expectEqual(
    "the App schema accepts the session",
    await appSchemaRejections(nodeRoot, store, [sessionID]),
    [],
  );

  // 3. rust-cli 冷读取：同样的 Artifact 引用，输入记录带着上传附件。
  const loaded = rust("read", dir, sessionID);
  expectEqual(
    "rust-cli reads the media back as the same artifacts",
    loaded.messages[0]?.content
      ?.filter((block) => block.type !== "text")
      .map(
        (block) =>
          block.source?.type === "artifact" && [
            block.type,
            `zcode-artifact://${block.source.artifact.session}/${block.source.artifact.id}`,
          ],
      ),
    [
      ["image", written.localUri],
      ["document", written.pdfUri],
      ["image", written.uri],
      ["image", written.jpegUri],
    ],
  );
  expectEqual(
    "rust-cli reads the origins back (the uploaded images keep no name, as in Node)",
    loaded.messages[0]?.content
      ?.filter((block) => block.type !== "text")
      .map((block) => block.origin ?? null),
    [{ file_name: "pic.png", path: written.localPath }, { file_name: "doc.pdf" }, null, null],
  );
  expectEqual(
    "rust-cli reads the uploads of the input record",
    loaded.inputs.map((record) => record.uploads?.map((upload) => upload.name)),
    [["small.png", "shot.png", "doc.pdf"]],
  );
}
