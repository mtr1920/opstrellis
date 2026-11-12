import type { Settings } from "./api";

export type LlmService = {
  id: string;
  label: string;
  provider: Settings["llm"]["provider"];
  /** null: the provider's own address. */
  baseUrl: string | null;
  defaultModel: string;
  /** Local servers that need no API key. */
  keyless?: boolean;
  /** Where to get a key (shown as text). */
  keyUrl?: string;
  models?: string[];
};

export const OTHER_SERVICE = "other";

export const LLM_SERVICES: LlmService[] = [
  { id: "openai", label: "OpenAI", provider: "openai", baseUrl: null, defaultModel: "gpt-6-luna", keyUrl: "platform.openai.com/api-keys", models: ["gpt-6-luna", "gpt-6.1-terra", "gpt-4o-mini"] },
  { id: "anthropic", label: "Anthropic", provider: "anthropic", baseUrl: null, defaultModel: "claude-haiku-4-5", keyUrl: "platform.claude.com/settings/keys", models: ["claude-haiku-5-5", "claude-haiku-4-5", "claude-sonnet-5-5"] },
  { id: "gemini", label: "Google Gemini", provider: "openai", baseUrl: "https://generativelanguage.googleapis.com/v1beta/openai", defaultModel: "gemini-3.5-flash-lite", keyUrl: "aistudio.google.com/apikey", models: ["gemini-3.5-flash-lite", "gemini-3.1-flash-lite", "gemini-3.6-flash"] },
  { id: "groq", label: "Groq", provider: "openai", baseUrl: "https://api.groq.com/openai/v1", defaultModel: "llama-3.1-8b-instant", keyUrl: "console.groq.com/keys", models: ["llama-3.1-8b-instant", "openai/gpt-oss-20b", "llama-3.3-70b-versatile"] },
  { id: "openrouter", label: "OpenRouter", provider: "openai", baseUrl: "https://openrouter.ai/api/v1", defaultModel: "openrouter/auto", keyUrl: "openrouter.ai/keys", models: ["openrouter/auto"] },
  { id: "mistral", label: "Mistral", provider: "openai", baseUrl: "https://api.mistral.ai/v1", defaultModel: "mistral-small-latest", keyUrl: "console.mistral.ai/api-keys", models: ["mistral-small-latest"] },
  { id: "deepseek", label: "DeepSeek", provider: "openai", baseUrl: "https://api.deepseek.com", defaultModel: "deepseek-flash", keyUrl: "platform.deepseek.com/api_keys", models: ["deepseek-flash", "deepseek-v4-pro"] },
  { id: "xai", label: "xAI", provider: "openai", baseUrl: "https://api.x.ai/v1", defaultModel: "grok-4.20-0309-non-reasoning", keyUrl: "console.x.ai/team/default/api-keys", models: ["grok-4.20-0309-non-reasoning", "grok-4.3"] },
  { id: "together", label: "Together AI", provider: "openai", baseUrl: "https://api.together.ai/v1", defaultModel: "Qwen/Qwen3.8-Flash", keyUrl: "api.together.ai/settings/projects/~current/api-keys", models: ["Qwen/Qwen3.8-Flash", "zai-org/GLM-5.3-Flash", "MiniMaxAI/MiniMax-M3.1-Flash"] },
  { id: "fireworks", label: "Fireworks", provider: "openai", baseUrl: "https://api.fireworks.ai/inference/v1", defaultModel: "accounts/fireworks/models/glm-5p3-flash", keyUrl: "app.fireworks.ai/settings/users/api-keys", models: ["accounts/fireworks/models/glm-5p3-flash"] },
  { id: "cerebras", label: "Cerebras", provider: "openai", baseUrl: "https://api.cerebras.ai/v1", defaultModel: "qwen-3.8-27b", keyUrl: "cloud.cerebras.ai", models: ["qwen-3.8-27b", "gpt-oss-120b"] },
  { id: "ollama", label: "Ollama (this computer)", provider: "openai", baseUrl: "http://localhost:11434/v1", defaultModel: "llama3.2", keyless: true, models: ["llama3.2", "qwen3:8b", "gpt-oss:20b"] },
  { id: "lmstudio", label: "LM Studio (this computer)", provider: "openai", baseUrl: "http://localhost:1234/v1", defaultModel: "local-model", keyless: true },
  { id: OTHER_SERVICE, label: "Other OpenAI-compatible", provider: "openai", baseUrl: null, defaultModel: "gpt-4o-mini" },
  { id: "cloud", label: "OpsTrellis Cloud", provider: "cloud", baseUrl: null, defaultModel: "auto" },
  { id: "local", label: "On this device (offline)", provider: "local", baseUrl: null, defaultModel: "spokenly-cleanup-1" },
];

const norm = (url: string | null) => (url ?? "").trim().replace(/\/+$/, "").toLowerCase();

/** The service a saved (provider, base URL) pair is: an unknown address is "Other OpenAI-compatible". */
export function serviceFor(llm: Settings["llm"]): LlmService {
  const base = llm.provider === "openai" ? norm(llm.baseUrl) : "";
  return LLM_SERVICES.find((s) => s.provider === llm.provider && s.id !== OTHER_SERVICE && norm(s.baseUrl) === base)
    ?? LLM_SERVICES.find((s) => s.id === OTHER_SERVICE)!;
}
