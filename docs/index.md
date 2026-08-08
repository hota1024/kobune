---
layout: home

hero:
  name: Minato
  text: A preview environment per git worktree
  tagline: Create a branch, and the environment is already running. Built for AI agents to drive.
  actions:
    - theme: brand
      text: Get started
      link: /guide/getting-started
    - theme: alt
      text: What is Minato?
      link: /guide/
    - theme: alt
      text: GitHub
      link: https://github.com/hota1024/minato

features:
  - title: One worktree, one environment
    details: An environment appears with its worktree and goes with it. That correspondence is the whole model, and it is the only thing you have to keep in your head.
  - title: No ports to remember
    details: Every service gets a stable URL — web.feature-auth.myapp.localhost — that survives restarts. Ports change underneath; the URL does not.
  - title: Scale to zero
    details: An untouched environment stops itself and wakes on the next request, in a second or two. Make as many worktrees as you like.
  - title: Agents can drive it
    details: Every command speaks --json and exits with a code that says what kind of failure it was. minato skill install teaches an agent the rest.
  - title: Your choice of runtime
    details: Docker and Apple Container behind one abstraction. Switch with a single line of minato.toml.
  - title: Share a preview
    details: Put a branch behind a Cloudflare Tunnel and send the link to a phone or a reviewer. Scale-to-zero still applies.
---
