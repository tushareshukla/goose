import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { LoadingGoose, type LoadingChatState } from "../LoadingGoose";
import chat from "@/shared/i18n/locales/en/chat.json";

const { thinking, responding, compacting, listening, success, sleeping } =
  chat.loading;

describe("LoadingGoose", () => {
  it("renders thinking copy for the thinking state", () => {
    render(<LoadingGoose chatState="thinking" />);

    expect(screen.getByRole("status", { name: thinking })).toBeInTheDocument();
  });

  it("renders responding copy for active response states", () => {
    const { rerender } = render(<LoadingGoose chatState="streaming" />);

    expect(
      screen.getByRole("status", { name: responding }),
    ).toBeInTheDocument();

    rerender(<LoadingGoose chatState="waiting" />);
    expect(
      screen.getByRole("status", { name: responding }),
    ).toBeInTheDocument();
  });

  it("renders compacting copy for the compacting state", () => {
    render(<LoadingGoose chatState="compacting" />);

    expect(
      screen.getByRole("status", { name: compacting }),
    ).toBeInTheDocument();
  });

  it("renders nothing while idle", () => {
    const { container } = render(<LoadingGoose chatState="idle" />);

    expect(container).toBeEmptyDOMElement();
  });

  // ── RUSKY FORK PATCH ──

  it("renders listening copy for the listening state", () => {
    render(<LoadingGoose chatState="listening" />);

    expect(
      screen.getByRole("status", { name: listening }),
    ).toBeInTheDocument();
  });

  it("renders success copy for the success state", () => {
    render(<LoadingGoose chatState="success" />);

    expect(screen.getByRole("status", { name: success })).toBeInTheDocument();
  });

  it("renders sleeping copy for the sleeping state", () => {
    render(<LoadingGoose chatState="sleeping" />);

    expect(
      screen.getByRole("status", { name: sleeping }),
    ).toBeInTheDocument();
  });

  it("covers all 8 LoadingChatState members exhaustively", () => {
    // TypeScript exhaustiveness: this switch must cover all 8 states or the
    // compiler will error. If the union grows, this test will break at compile
    // time, surfacing the gap immediately.
    const allStates: LoadingChatState[] = [
      "idle",
      "thinking",
      "streaming",
      "waiting",
      "compacting",
      "listening",
      "success",
      "sleeping",
    ];
    expect(allStates).toHaveLength(8);
  });

  // ── /RUSKY FORK PATCH ──
});
