-- Bundled todo tool: session-isolated task list
-- State is stored in module-level tables, keyed by session ID.

local lists = {} -- { [session_id] = { {id=1, text="...", status="pending"}, ... } }

local function get_list()
    local session_id = genesis.session.id
    if not lists[session_id] then
        lists[session_id] = {}
    end
    return lists[session_id]
end

local function next_id(list)
    local max = 0
    for _, item in ipairs(list) do
        if item.id > max then max = item.id end
    end
    return max + 1
end

genesis.register_tool({
    name = "todo",
    description = "Manage a session-scoped todo list. Actions: add, update, list, clear.",
    parameters = {
        action = {
            type = "string",
            description = "Action to perform: add, update, list, or clear",
            required = true,
        },
        text = {
            type = "string",
            description = "Text for the todo item (required for 'add')",
        },
        id = {
            type = "string",
            description = "Item ID (required for 'update')",
        },
        status = {
            type = "string",
            description = "New status for 'update': pending, in_progress, or done",
        },
    },
    run = function(args)
        local action = args.action:lower()
        local list = get_list()

        if action == "add" then
            if not args.text or #args.text == 0 then
                error("'text' is required for 'add' action")
            end
            local id = next_id(list)
            table.insert(list, { id = id, text = args.text, status = "pending" })
            return "Added item #" .. id .. ": " .. args.text

        elseif action == "update" then
            if not args.id then
                error("'id' is required for 'update' action")
            end
            local target_id = tonumber(args.id)
            if not target_id then
                error("'id' must be a number")
            end
            if not args.status then
                error("'status' is required for 'update' action")
            end
            local new_status = args.status
            if new_status ~= "pending" and new_status ~= "in_progress" and new_status ~= "done" then
                error("'status' must be one of: pending, in_progress, done")
            end
            for _, item in ipairs(list) do
                if item.id == target_id then
                    item.status = new_status
                    return "Updated item #" .. target_id .. " to " .. new_status
                end
            end
            error("Item #" .. target_id .. " not found")

        elseif action == "list" then
            if #list == 0 then
                return "No items in the todo list."
            end
            local markers = { pending = "[ ]", in_progress = "[~]", done = "[x]" }
            local lines = {}
            local counts = { pending = 0, in_progress = 0, done = 0 }
            for _, item in ipairs(list) do
                local marker = markers[item.status] or "[ ]"
                table.insert(lines, marker .. " #" .. item.id .. ": " .. item.text)
                counts[item.status] = (counts[item.status] or 0) + 1
            end
            table.insert(lines, "")
            table.insert(lines, string.format(
                "Summary: %d pending, %d in progress, %d done (%d total)",
                counts.pending, counts.in_progress, counts.done, #list
            ))
            return table.concat(lines, "\n")

        elseif action == "clear" then
            lists[genesis.session.id] = {}
            return "Todo list cleared."

        else
            error("Unknown action: " .. action .. ". Use: add, update, list, or clear")
        end
    end,
})
