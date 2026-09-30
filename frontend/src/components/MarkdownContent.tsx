import Markdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";

/**
 * Only web, mail and in-app relative links stay clickable. Anything else,
 * including `javascript:` and `data:` URLs, is dropped and its text is shown
 * without a link.
 */
export function safeMarkdownUrl(value: string): string {
  const url = value.trim();
  const scheme = /^([a-z][a-z0-9+.-]*):/i.exec(url)?.[1]?.toLowerCase();
  if (scheme === undefined) {
    // Protocol-relative URLs name another host, so they are not relative.
    return url.startsWith("//") ? "" : url;
  }
  return ["http", "https", "mailto"].includes(scheme) ? url : "";
}

const components: Components = {
  a: ({ href, children, node: _node, ...props }) =>
    href
      ? (
        <a
          {...props}
          href={href}
          target="_blank"
          rel="noopener noreferrer nofollow"
        >
          {children}
        </a>
      )
      : <span className="markdown-inert-link">{children}</span>,
  // Images would load remote content, so only their description is shown.
  img: ({ alt }) => (
    <span className="markdown-image-text">
      {alt ? `[Image: ${alt}]` : "[Image]"}
    </span>
  ),
  table: ({ node: _node, ...props }) => (
    <div className="markdown-table" tabIndex={0}>
      <table {...props} />
    </div>
  ),
  pre: ({ node: _node, ...props }) => <pre tabIndex={0} {...props} />,
};

/**
 * Renders agent plans, reports and feedback as Markdown with GitHub tables
 * and task lists. Raw HTML is never rendered.
 */
export function MarkdownContent(
  { text, className = "" }: { text: string; className?: string },
) {
  return (
    <div className={`markdown-content ${className}`.trim()}>
      <Markdown
        remarkPlugins={[remarkGfm]}
        skipHtml
        urlTransform={safeMarkdownUrl}
        components={components}
      >
        {text}
      </Markdown>
    </div>
  );
}
