# nars-agent web UI

React 19 + TypeScript + Vite. No UI framework; the app is a pure projection of
the server's SSE event stream.

```bash
npm install
npm run dev      # http://localhost:5173, proxies /api to 127.0.0.1:8787
npm run build    # type-check + build into web/dist
```

`narsd` serves `web/dist` automatically when the directory exists.

## Layout

- `src/api.ts` — fetch client, token handling, SSE URL.
- `src/types.ts` — protocol types mirrored from `nars-agent-ir`.
- `src/store.ts` — reducer that folds `AgentEvent`s into run state and timeline.
- `src/components/` — sidebar, timeline, tool cards, diffs, approval, inspector.

The reducer deduplicates by event `seq`, so EventSource reconnects can replay the
full history safely.
