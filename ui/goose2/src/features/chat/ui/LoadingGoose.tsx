import { useTranslation } from "react-i18next";
import { motion, useReducedMotion } from "motion/react";
import { Shimmer } from "@/shared/ui/ai-elements/shimmer";

// ── RUSKY FORK PATCH ──
// Extended LoadingChatState — 5 upstream states + 3 Rusky additions.
// The 3 additions map to HuskyState variants from SPEC-030.
export type LoadingChatState =
  | "idle" // upstream — Husky: idle
  | "thinking" // upstream — Husky: thinking
  | "streaming" // upstream — Husky: responding
  | "waiting" // upstream — Husky: responding (same visual)
  | "compacting" // upstream — Husky: thinking (compaction = cognitive work)
  | "listening" // RUSKY-ADDED — Husky: listening (microphone active)
  | "success" // RUSKY-ADDED — Husky: success (action completed)
  | "sleeping"; // RUSKY-ADDED — Husky: sleeping (idle > auto-sleep threshold)
// ── /RUSKY FORK PATCH ──

interface LoadingGooseProps {
  chatState?: LoadingChatState;
}

const LOADING_FADE_S = 0.45;
const LOADING_SHIMMER_S = 3;
const LOADING_SHIMMER_SPREAD = 3;
const LOADING_SHIMMER_DELAY_S = 0.35;
const LOADING_SHIMMER_REPEAT_DELAY_S = 0.9;

// ── RUSKY FORK PATCH ──
// Extended message key map — covers all 8 members of LoadingChatState.
const MESSAGE_KEY_BY_STATE: Record<
  Exclude<LoadingChatState, "idle">,
  "thinking" | "responding" | "compacting" | "listening" | "success" | "sleeping"
> = {
  thinking: "thinking",
  streaming: "responding",
  waiting: "responding",
  compacting: "compacting",
  listening: "listening", // RUSKY-ADDED
  success: "success", // RUSKY-ADDED
  sleeping: "sleeping", // RUSKY-ADDED
};
// ── /RUSKY FORK PATCH ──

export function LoadingGoose({ chatState = "idle" }: LoadingGooseProps) {
  const { t } = useTranslation("chat");
  const shouldReduceMotion = useReducedMotion();
  if (chatState === "idle") {
    return null;
  }

  const message = t(`loading.${MESSAGE_KEY_BY_STATE[chatState]}`);

  return (
    <motion.div
      className="px-4"
      role="status"
      aria-label={message}
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      exit={{ opacity: 0 }}
      transition={{ duration: shouldReduceMotion ? 0 : LOADING_FADE_S }}
    >
      <div className="max-w-3xl mx-auto w-full">
        <div className="py-2 text-xs text-muted-foreground">
          {shouldReduceMotion ? (
            <span>{message}</span>
          ) : (
            <Shimmer
              as="span"
              className="text-xs"
              tone="soft"
              delay={LOADING_SHIMMER_DELAY_S}
              duration={LOADING_SHIMMER_S}
              spread={LOADING_SHIMMER_SPREAD}
              repeatDelay={LOADING_SHIMMER_REPEAT_DELAY_S}
            >
              {message}
            </Shimmer>
          )}
        </div>
      </div>
    </motion.div>
  );
}
