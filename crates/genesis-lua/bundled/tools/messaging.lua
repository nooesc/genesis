-- Bundled messaging tool: multi-platform message dispatch
-- Supports: slack, telegram, discord, whatsapp, homeassistant

-- UTF-8-safe message splitting for Telegram's 4096-char limit
local function split_message(text, max_len)
    if #text <= max_len then return { text } end
    local chunks = {}
    local remaining = text
    while #remaining > 0 do
        if #remaining <= max_len then
            table.insert(chunks, remaining)
            break
        end
        -- Try to split at a newline within the limit
        local cut = max_len
        local newline = remaining:sub(1, max_len):find("\n[^\n]*$")
        if newline and newline > max_len * 0.5 then
            cut = newline
        else
            -- Try word boundary
            local space = remaining:sub(1, max_len):find("%s[^%s]*$")
            if space and space > max_len * 0.5 then
                cut = space
            end
        end
        -- Retreat to a UTF-8 character boundary so we never split a
        -- multi-byte codepoint (Lua 5.4 utf8 library).
        local safe_cut = cut
        while safe_cut > 0 do
            local byte = remaining:byte(safe_cut)
            if byte and (byte < 0x80 or byte >= 0xC0) then
                break -- start of a character or ASCII
            end
            safe_cut = safe_cut - 1
        end
        if safe_cut > 0 then cut = safe_cut end
        table.insert(chunks, remaining:sub(1, cut))
        remaining = remaining:sub(cut + 1)
    end
    return chunks
end

local function truncate(text, limit)
    if #text <= limit then return text end
    return text:sub(1, limit - 3) .. "..."
end

local function send_slack(channel, message, thread_id)
    local token = genesis.config.env("SLACK_BOT_TOKEN")
    if not token then error("SLACK_BOT_TOKEN not configured") end

    local body = {
        channel = channel,
        text = message,
    }
    if thread_id and #thread_id > 0 then
        body.thread_ts = thread_id
    end

    local resp = genesis.http.request("https://slack.com/api/chat.postMessage", {
        method = "POST",
        headers = {
            ["Content-Type"] = "application/json",
            ["Authorization"] = "Bearer " .. token,
        },
        body = genesis.json.encode(body),
    })
    if resp.status ~= 200 then
        error("Slack API error (HTTP " .. resp.status .. "): " .. resp.body)
    end
    local data = genesis.json.decode(resp.body)
    if not data.ok then
        error("Slack API error: " .. (data.error or "unknown"))
    end
    return "Message sent to Slack channel " .. channel
end

local function send_telegram(chat_id, message, thread_id)
    local token = genesis.config.env("TELEGRAM_BOT_TOKEN")
    if not token then error("TELEGRAM_BOT_TOKEN not configured") end

    local chunks = split_message(message, 4096)
    local last_message_id = nil

    for i, chunk in ipairs(chunks) do
        local body = {
            chat_id = chat_id,
            text = chunk,
        }
        if i == 1 and thread_id and #thread_id > 0 then
            body.reply_to_message_id = tonumber(thread_id)
        end

        local url = "https://api.telegram.org/bot" .. token .. "/sendMessage"
        local resp = genesis.http.request(url, {
            method = "POST",
            headers = { ["Content-Type"] = "application/json" },
            body = genesis.json.encode(body),
        })
        if resp.status ~= 200 then
            error("Telegram API error (HTTP " .. resp.status .. "): " .. resp.body)
        end
        local data = genesis.json.decode(resp.body)
        if not data.ok then
            error("Telegram API error: " .. (data.description or "unknown"))
        end
        last_message_id = data.result and data.result.message_id
    end
    return "Message sent to Telegram chat " .. chat_id
        .. (#chunks > 1 and (" (" .. #chunks .. " parts)") or "")
end

local function send_discord(channel_id, message)
    local token = genesis.config.env("DISCORD_BOT_TOKEN")
    if not token then error("DISCORD_BOT_TOKEN not configured") end

    local truncated = truncate(message, 2000)
    local resp = genesis.http.request(
        "https://discord.com/api/v10/channels/" .. channel_id .. "/messages",
        {
            method = "POST",
            headers = {
                ["Content-Type"] = "application/json",
                ["Authorization"] = "Bot " .. token,
            },
            body = genesis.json.encode({ content = truncated }),
        }
    )
    if resp.status < 200 or resp.status >= 300 then
        error("Discord API error (HTTP " .. resp.status .. "): " .. resp.body)
    end
    return "Message sent to Discord channel " .. channel_id
end

local function send_whatsapp(recipient, message)
    local token = genesis.config.env("WHATSAPP_TOKEN")
    if not token then error("WHATSAPP_TOKEN not configured") end
    local phone_id = genesis.config.env("WHATSAPP_PHONE_NUMBER_ID")
    if not phone_id then error("WHATSAPP_PHONE_NUMBER_ID not configured") end

    local truncated = truncate(message, 4096)
    local url = "https://graph.facebook.com/v21.0/" .. phone_id .. "/messages"
    local resp = genesis.http.request(url, {
        method = "POST",
        headers = {
            ["Content-Type"] = "application/json",
            ["Authorization"] = "Bearer " .. token,
        },
        body = genesis.json.encode({
            messaging_product = "whatsapp",
            to = recipient,
            type = "text",
            text = { body = truncated },
        }),
    })
    if resp.status ~= 200 then
        error("WhatsApp API error (HTTP " .. resp.status .. "): " .. resp.body)
    end
    return "Message sent to WhatsApp " .. recipient
end

local function send_homeassistant(target, message)
    local url = genesis.config.env("HOMEASSISTANT_URL")
    if not url then error("HOMEASSISTANT_URL not configured") end
    local token = genesis.config.env("HOMEASSISTANT_LONG_LIVED_TOKEN")
    if not token then error("HOMEASSISTANT_LONG_LIVED_TOKEN not configured") end

    -- Parse "domain.service" format, default to "notify.{target}"
    local domain, service
    if target:find("%.") then
        domain, service = target:match("^([^%.]+)%.(.+)$")
    else
        domain = "notify"
        service = target
    end

    local endpoint = url:gsub("/$", "") .. "/api/services/" .. domain .. "/" .. service
    local resp = genesis.http.request(endpoint, {
        method = "POST",
        headers = {
            ["Content-Type"] = "application/json",
            ["Authorization"] = "Bearer " .. token,
        },
        body = genesis.json.encode({
            title = "Genesis Agent",
            message = message,
        }),
    })
    if resp.status < 200 or resp.status >= 300 then
        error("Home Assistant error (HTTP " .. resp.status .. "): " .. resp.body)
    end
    return "Message sent via Home Assistant to " .. target
end

genesis.register_tool({
    name = "send_message",
    description = "Send a message to a platform channel. Supported platforms: slack, telegram, discord, whatsapp, homeassistant.",
    approval = "always",
    parameters = {
        platform = {
            type = "string",
            description = "Target platform (slack, telegram, discord, whatsapp, homeassistant)",
            required = true,
        },
        channel = {
            type = "string",
            description = "Channel/chat ID or recipient identifier",
            required = true,
        },
        message = {
            type = "string",
            description = "Message content to send",
            required = true,
        },
        thread_id = {
            type = "string",
            description = "Optional thread/reply ID for threading support",
        },
    },
    run = function(args)
        local platform = args.platform:lower()
        if platform == "slack" then
            return send_slack(args.channel, args.message, args.thread_id)
        elseif platform == "telegram" then
            return send_telegram(args.channel, args.message, args.thread_id)
        elseif platform == "discord" then
            return send_discord(args.channel, args.message)
        elseif platform == "whatsapp" then
            return send_whatsapp(args.channel, args.message)
        elseif platform == "homeassistant" or platform == "home_assistant" then
            return send_homeassistant(args.channel, args.message)
        else
            error("Unsupported platform: " .. platform
                .. ". Use: slack, telegram, discord, whatsapp, homeassistant")
        end
    end,
})
