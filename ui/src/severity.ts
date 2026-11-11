// One tone for each alarm severity (the ids of EdgeAlarm.severity in packages/edge), for every screen.
import type { Badge } from "@ferrum/kit";

type Tone = NonNullable<Parameters<typeof Badge>[0]["tone"]>;

export const SEVERITY_TONE: Readonly<Record<string, Tone>> = { Critical: "red", Major: "red", Minor: "amber", Warning: "amber", Info: "blue" };

// An id nobody mapped is amber, never a calm colour: an unknown severity must not look harmless.
export const severityTone = (severity: string): Tone => (Object.prototype.hasOwnProperty.call(SEVERITY_TONE, severity) ? SEVERITY_TONE[severity]! : "amber");
