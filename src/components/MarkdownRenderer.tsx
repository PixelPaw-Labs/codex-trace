import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { Prism as SyntaxHighlighter } from "react-syntax-highlighter";
import { oneDark } from "react-syntax-highlighter/dist/esm/styles/prism";
import type { Components } from "react-markdown";
import { formatJson } from "../lib/format";

function isPureJson(s: string): boolean {
  const t = s.trimStart();
  if (t[0] !== "{" && t[0] !== "[") return false;
  try {
    JSON.parse(s);
    return true;
  } catch {
    return false;
  }
}

/** Module level, not defined during render: a fresh component identity each
 * render makes React unmount and rebuild every highlighted block, which throws
 * away the highlighter's work and any scroll position inside it. */
const CodeBlock: Components["code"] = ({ className, children }) => {
  const match = /language-(\w+)/.exec(className ?? "");
  const lang = match ? match[1] : "";
  const code = String(children).replace(/\n$/, "");
  if (lang) {
    return (
      <SyntaxHighlighter language={lang} style={oneDark} PreTag="div">
        {code}
      </SyntaxHighlighter>
    );
  }
  return <code className={className}>{children}</code>;
};

const MARKDOWN_COMPONENTS: Components = { code: CodeBlock };

const REMARK_PLUGINS = [remarkGfm];

export function MarkdownRenderer({ content }: { content: string }) {
  if (isPureJson(content)) {
    return (
      <SyntaxHighlighter language="json" style={oneDark} PreTag="div">
        {formatJson(content)}
      </SyntaxHighlighter>
    );
  }

  return (
    <ReactMarkdown remarkPlugins={REMARK_PLUGINS} components={MARKDOWN_COMPONENTS}>
      {content}
    </ReactMarkdown>
  );
}
