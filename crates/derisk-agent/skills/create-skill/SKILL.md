---
name: create-skill
description: Helps you create new agent skills for the derisk Agent and Claude Code. Use this to create a skill, ask about SKILL.md, or package reusable agent instructions.
---

# Creating an agent skill

Use this skill when the user wants to create, edit, or understand agent skills.

## What is a Skill?

A skill is a reusable set of instructions that an agent can load on demand. Each skill lives in its own directory and is defined by a `SKILL.md` file with YAML frontmatter.

## Where Skills Live

The derisk Agent runs Claude Code, which finds skills in two places:

| Scope | Path | When to use |
|-------|------|-------------|
| Personal | `~/.claude/skills/<skill-name>/SKILL.md` | Personal skills, available in every project |
| Project | `<project>/.claude/skills/<skill-name>/SKILL.md` | Project-specific skills, shared with collaborators through version control |

Prefer the project when the skill is specific to a repository. Prefer personal when the skill is a workflow the user wants everywhere. The skills that come with the Agent are read-only; to change one, copy its folder into one of these places under a new name.

## SKILL.md Format

Every `SKILL.md` must start with YAML frontmatter between `---` delimiters:

```markdown
---
name: my-skill-name
description: A clear, specific description of what this skill does and when to use it.
---

# Skill Title

Instructions for the agent go here. Write them as if you're telling the agent
what to do when this skill is activated.
```

### Required Frontmatter Fields

- **`name`** (required): Must be 1–64 characters, lowercase alphanumeric with single-hyphen separators. Must match the containing directory name exactly. Regex: `^[a-z0-9]+(-[a-z0-9]+)*$`
- **`description`** (required): Must be 1–1024 characters. This is what the agent sees when deciding whether to use the skill — make it specific and actionable.

### Optional Frontmatter Fields

- **`disable-model-invocation`**: When set to `true`, the agent does not load the skill by itself; the user invokes it by name as a slash command. Useful for skills that should only run when explicitly requested.

## Naming Rules

The skill name must:
- Be lowercase letters and numbers only, with single hyphens as separators
- Not start or end with `-`
- Not contain consecutive `--`
- Match the directory name that contains the `SKILL.md`

Good: `git-release`, `pr-review`, `rust-patterns`
Bad: `Git-Release`, `pr--review`, `-my-skill`, `my_skill`

## Writing Good Skill Instructions

The body of the SKILL.md (after the frontmatter) contains the instructions the agent will follow. Guidelines:

1. **Be direct**: Write instructions as if talking to the agent. "Do X", "Check Y", "Ask the user about Z".
2. **Be specific**: Include concrete file paths, commands, formats, and patterns.
3. **Include when-to-use guidance**: Help the agent understand the right context for this skill.
4. **Reference supporting files**: Skills can include additional files in their directory. Reference them with relative paths (e.g., `templates/component.tsx`). The agent can read these files when the skill is activated.
5. **Keep descriptions actionable**: The `description` field is the agent's primary signal for whether to load this skill. "Helps with code" is too vague. "Generate React components following the project's design system patterns" is specific.
6. **Keep instructions focused**: Limit instructions to those relevant to the skill itself. Avoid duplicating instructions from CLAUDE.md, the project's instructions or other skills.

## Supporting Files

A skill directory can contain additional files beyond `SKILL.md`:

```
~/.claude/skills/react-component/
├── SKILL.md
├── templates/
│   ├── component.tsx
│   └── test.tsx
└── examples/
    └── button.tsx
```

Reference these in the skill body by their path relative to the skill's folder.

## Step-by-Step: Creating a Skill

1. Decide on scope (personal or project) based on the user's needs.
2. Choose a descriptive, hyphenated name.
3. Create the directory, write the `SKILL.md` with frontmatter and instructions, and add any supporting files (templates, examples, references).
4. Tell the user where the skill is. It is picked up from the next chat on; a running chat does not see it.
