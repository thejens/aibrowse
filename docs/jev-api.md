# Jev API reference (for the A4 navigator)

Condensed from https://docs.typesafe.ai/api (read with aibrowse, 2026-10-05).

## Endpoint

```
POST https://api.typesafe.ai/v1/systemone
Authorization: Bearer $JEV_API_KEY        # in .env
Content-Type: application/json
```

## Request

```jsonc
{
  "model": "jev-latest",
  "state": "...",            // string | object | array: the page model goes here
  "questions": {              // map of name -> Question; answers come back under the same keys
    "goal_reached": { "type": "noul", "instructions": "...", "criteria": { "true": "...", "false": "..." } },
    "next_action":  { "type": "choice", "instructions": "...", "criteria": { "e12": "Link 'Terms'", "e40": null } },
    "progress":     { "type": "score", "instructions": "...", "criteria": ["far", "close", "there"] }
  }
}
```

- `instructions` can be a string, an object or an array. Put data in named fields and
  refer to them in backticks, e.g. `"question": "Which option best advances `goal`?"`.
- `choice`: at most **255 options**. The navigator's candidate list must stay under that.
  The criteria value can be `null` when the key is self-explanatory.
- `score`: 2–10 ordered levels.

## Response

```jsonc
{
  "model": "jev-1.13.0",
  "answers": {
    "goal_reached": { "type": "noul", "noul": 0.95 },
    "next_action":  { "type": "choice", "choice": "e12", "probabilities": { "e12": 0.88, ... }, "confidence": 0.81 },
    "progress":     { "type": "score", "score": 1.05, "legend": { "0": "far", ... }, "probabilities": { ... }, "confidence": 0.92 }
  },
  "usage": { "input_tokens": 296, "output_tokens": 20 }
}
```

## Errors

| Status | Meaning | Handling |
|---|---|---|
| 401 | Bad or missing key | Fail fast |
| 422 | Validation (the body names the field) | Fail, and surface the field |
| 429 | Rate limited | Exponential backoff |
| 529 | Overloaded | Exponential backoff |

## Design notes for the navigator

- Ask every question in one request (speculative fan-out). It is far cheaper and faster
  than sequential calls.
- Use refs as `choice` option keys, so the answer maps straight to an `act` call.
- `confidence` (choice and score) drives the hand-back threshold. For `noul`, use the
  probability itself.
