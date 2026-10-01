import { Component, type ReactNode, useEffect, useRef } from "react";
import { TechnicalDetails } from "./ErrorNotice";

interface RenderFailure {
  errorType: string;
  occurredAt: string;
}

interface AppErrorBoundaryProps {
  children: ReactNode;
}

interface AppErrorBoundaryState {
  failure?: RenderFailure;
}

const builtInErrorTypes = [
  TypeError,
  RangeError,
  ReferenceError,
  SyntaxError,
  URIError,
];

// React's default onCaughtError logs the full error to the console. The page
// shows only fixed labels because messages and stacks can quote task text.
function errorTypeLabel(error: unknown): string {
  if (!(error instanceof Error)) return "Non-Error value";
  return builtInErrorTypes.find((type) => error instanceof type)?.name ??
    "Error";
}

function RenderFailureNotice({ failure }: { failure: RenderFailure }) {
  const heading = useRef<HTMLHeadingElement>(null);
  useEffect(() => heading.current?.focus(), []);
  return (
    <main className="render-failure">
      <section className="panel">
        <h1 ref={heading} tabIndex={-1}>The dashboard stopped working</h1>
        <p>
          Something went wrong while showing this page. Choose Reload page to
          open the dashboard again.
        </p>
        <p>
          If you had just chosen an action, check whether it completed before
          trying it again. If this keeps happening, open Technical details and
          include them when reporting the problem.
        </p>
        <div className="button-row">
          <button
            type="button"
            className="primary"
            onClick={() => window.location.reload()}
          >
            Reload page
          </button>
        </div>
        <TechnicalDetails>
          <p>
            Dashboard render error · {failure.errorType} · {failure.occurredAt}
          </p>
          <p>
            The full error is in this browser's developer console until the
            page reloads. It can include private task details, so review it
            before sharing.
          </p>
        </TechnicalDetails>
      </section>
    </main>
  );
}

export class AppErrorBoundary
  extends Component<AppErrorBoundaryProps, AppErrorBoundaryState> {
  state: AppErrorBoundaryState = {};

  static getDerivedStateFromError(error: unknown): AppErrorBoundaryState {
    return {
      failure: {
        errorType: errorTypeLabel(error),
        occurredAt: new Date().toISOString(),
      },
    };
  }

  render() {
    return this.state.failure
      ? <RenderFailureNotice failure={this.state.failure} />
      : this.props.children;
  }
}
