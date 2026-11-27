import { Rpc } from "@opencode/plugin/rpc"

/**
 * Typed contract between the OpenCode server part and its TUI/GUI parts.
 *
 * Every method maps to one read-only control-socket call (or one explicitly
 * user-triggered project action). The plugins never talk to the socket
 * directly: the server part owns the daemon connection, and the contract is
 * the only surface the views can use. Registration/approval stays CLI-only.
 */
const str = { type: "string" }
const repoPath = { type: "string", minLength: 1 }

const object = (properties = {}, required = []) => ({
  type: "object",
  properties,
  required,
  additionalProperties: true,
})

const anyObject = () => ({ type: "object", additionalProperties: true })
const anyArray = () => ({ type: "array", items: {} })

const controlError = object(
  {
    message: str,
    method: str,
  },
  ["message", "method"],
)

export const Nexus = Rpc.define({
  id: "nexuscontext",
  methods: {
    status: {
      input: object(),
      output: anyObject(),
      errors: { control: controlError },
    },
    sources: {
      input: object(),
      output: anyObject(),
      errors: { control: controlError },
    },
    projects: {
      input: object(),
      output: anyArray(),
      errors: { control: controlError },
    },
    search: {
      input: object(
        {
          repo_path: repoPath,
          pattern: str,
          limit: { type: "integer", minimum: 1, maximum: 200 },
        },
        ["repo_path", "pattern"],
      ),
      output: anyArray(),
      errors: { control: controlError },
    },
    architecture: {
      input: object({ repo_path: repoPath }, ["repo_path"]),
      output: anyObject(),
      errors: { control: controlError },
    },
    graph: {
      input: object(
        {
          repo_path: repoPath,
          function_name: str,
          direction: { type: "string", enum: ["inbound", "outbound"] },
          depth: { type: "integer", minimum: 1, maximum: 10 },
        },
        ["repo_path", "function_name"],
      ),
      output: anyObject(),
      errors: { control: controlError },
    },
    review: {
      input: object(
        {
          repo_path: str,
          limit: { type: "integer", minimum: 1, maximum: 200 },
        },
        [],
      ),
      output: anyObject(),
      errors: { control: controlError },
    },
    stats: {
      input: object(),
      output: anyObject(),
      errors: { control: controlError },
    },
    logs: {
      input: object({ limit: { type: "integer", minimum: 1, maximum: 500 } }, []),
      output: anyObject(),
      errors: { control: controlError },
    },
    ledger: {
      input: object(),
      output: anyObject(),
      errors: { control: controlError },
    },
    reindex: {
      input: object({ repo_path: repoPath }, ["repo_path"]),
      output: anyObject(),
      errors: { control: controlError },
    },
    importProject: {
      input: object({ repo_path: repoPath }, ["repo_path"]),
      output: anyObject(),
      errors: { control: controlError },
    },
    exportProject: {
      input: object({ repo_path: repoPath }, ["repo_path"]),
      output: anyObject(),
      errors: { control: controlError },
    },
    removeProject: {
      // Destructive: the view must pass an explicit confirmation token.
      input: object(
        {
          repo_path: repoPath,
          confirm: { type: "string", enum: ["delete"] },
        },
        ["repo_path", "confirm"],
      ),
      output: anyObject(),
      errors: {
        control: controlError,
        confirmation_required: object({ repo_path: repoPath }, ["repo_path"]),
      },
    },
  },
  events: {},
})
