---
layout: home

hero:
  name: Kobune
  text: A preview environment per git worktree
  tagline: Create a branch, and the environment is already running at a URL of its own. Made to be driven by an agent as readily as by you.
  actions:
    - theme: brand
      text: Get started
      link: /guide/getting-started
    - theme: alt
      text: What is Kobune?
      link: /guide/
    - theme: alt
      text: GitHub
      link: https://github.com/hota1024/kobune
  install:
    command: "curl -fsSL https://minato.1024.works/install.sh | sh"
    copy: Copy
    copied: Copied

specs:
  title: What you get
  items:
    - label: one worktree, one environment
      body: An environment appears with its worktree and goes with it. That correspondence is the whole model.
    - label: no ports to remember
      body: Every service gets a URL that survives restarts — web.feature-auth.myapp.localhost — while the port underneath changes as it likes.
    - label: scale to zero
      body: An untouched environment stops itself and wakes on the next request, in a second or two.
    - label: agents can drive it
      body: Every command speaks --json and exits with a code that says what kind of failure it was.
    - label: docker or apple container
      body: Two runtimes behind one abstraction, switched with a single line of kobune.toml.
    - label: share over a tunnel
      body: Put a branch behind a Cloudflare Tunnel and send the link to a phone or a reviewer.

notes:
  title: What it is not
  items:
    - lead: Not a production deployment tool.
      body: Everything here assumes a development machine and a person who owns it.
    - lead: Not a container runtime.
      body: Docker or Apple Container does that work; Kobune arranges it.
    - lead: Not a replacement for compose.
      body: If one stack on one branch is all you need, compose is simpler and you should keep using it.
  link: /guide/how-it-works
  linkText: How it works
---
