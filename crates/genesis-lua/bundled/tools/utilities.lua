-- Bundled utilities: echo, think, clarify
-- These are simple tools with no external dependencies.

genesis.register_tool({
    name = "echo",
    description = "Echoes a message back into the runtime for local tool testing.",
    parameters = {
        message = {
            type = "string",
            description = "The message to echo back",
            required = true,
        },
    },
    run = function(args)
        return args.message
    end,
})

genesis.register_tool({
    name = "think",
    description = "Use this tool to think through complex problems step-by-step. Your thoughts are recorded in the conversation but not shown to the user. Use for planning, reasoning, and working through tricky logic before taking action.",
    parameters = {
        thought = {
            type = "string",
            description = "Your internal reasoning or analysis",
            required = true,
        },
    },
    run = function(args)
        -- The value is in the tool call arguments preserved in conversation
        -- history, not in the result. Return empty string.
        return ""
    end,
})

genesis.register_tool({
    name = "clarify",
    description = "Ask the user a clarifying question before proceeding. Use when requirements are ambiguous or more context is needed.",
    parameters = {
        question = {
            type = "string",
            description = "The clarifying question to ask",
            required = true,
        },
        choices = {
            type = "string",
            description = "Optional comma-separated list of choices",
        },
    },
    run = function(args)
        local content = "[Clarification needed]\n" .. args.question
        if args.choices and #args.choices > 0 then
            local options = {}
            for opt in args.choices:gmatch("[^,]+") do
                table.insert(options, opt:match("^%s*(.-)%s*$"))
            end
            if #options > 0 then
                content = content .. "\n\nOptions:"
                for i, option in ipairs(options) do
                    content = content .. "\n  " .. i .. ". " .. option
                end
            end
        end
        -- Return with metadata so agent loop triggers ClarificationNeeded event
        return { content = content, metadata = { requires_input = "true" } }
    end,
})
