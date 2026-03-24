-- Bundled web search tool: Brave Search API + DuckDuckGo fallback
-- Primitives: http, json, config

local MAX_RESULTS = 10

local function url_encode(str)
    return str:gsub("([^%w%-%.%_%~ ])", function(c)
        return string.format("%%%02X", string.byte(c))
    end):gsub(" ", "+")
end

local function search_brave(query, count, api_key)
    local url = "https://api.search.brave.com/res/v1/web/search?q="
        .. url_encode(query)
        .. "&count=" .. count
    local resp = genesis.http.request(url, {
        method = "GET",
        headers = {
            ["Accept"] = "application/json",
            ["Accept-Encoding"] = "gzip",
            ["X-Subscription-Token"] = api_key,
        },
    })
    if resp.status ~= 200 then
        error("API returned status " .. resp.status)
    end
    local body = genesis.json.decode(resp.body)
    local web = body and body.web and body.web.results
    if not web or #web == 0 then
        return "Search results for: " .. query .. "\n\nNo results found."
    end
    local lines = {}
    for i, r in ipairs(web) do
        if i > count then break end
        local title = r.title or "(no title)"
        local u = r.url or ""
        local desc = r.description or "(no description)"
        table.insert(lines, i .. ". " .. title .. "\n   " .. u .. "\n   " .. desc)
    end
    return "Search results for: " .. query .. "\n\n" .. table.concat(lines, "\n\n")
end

local function search_ddg(query, count)
    local url = "https://api.duckduckgo.com/?q="
        .. url_encode(query)
        .. "&format=json&no_html=1"
    local resp = genesis.http.request(url, { method = "GET" })
    if resp.status ~= 200 then
        error("DuckDuckGo API error (HTTP " .. resp.status .. ")")
    end
    local body = genesis.json.decode(resp.body)
    local results = {}
    -- Abstract (main result)
    local abstract_text = body.AbstractText or ""
    if #abstract_text > 0 then
        local source = body.AbstractSource or ""
        local aurl = body.AbstractURL or ""
        table.insert(results, "1. " .. source .. " (" .. aurl .. ")\n   " .. aurl .. "\n   " .. abstract_text)
    end
    -- Related topics
    local topics = body.RelatedTopics or {}
    for _, topic in ipairs(topics) do
        if #results >= count then break end
        local text = topic.Text
        local furl = topic.FirstURL
        if text and furl then
            local idx = #results + 1
            table.insert(results, idx .. ". " .. text .. "\n   " .. furl)
        end
    end
    if #results == 0 then
        return "Search results for: " .. query
            .. "\n\nNo instant results available. Try using web_request to fetch a specific URL directly."
    end
    return "Search results for: " .. query .. "\n\n" .. table.concat(results, "\n\n")
end

genesis.register_tool({
    name = "web_search",
    description = "Searches the web and returns relevant results. Uses Brave Search API when BRAVE_API_KEY is set, otherwise falls back to DuckDuckGo.",
    parameters = {
        query = {
            type = "string",
            description = "The search query.",
            required = true,
        },
        count = {
            type = "integer",
            description = "Number of results to return (default: 5, max: 10).",
        },
    },
    run = function(args)
        if not args.query or args.query:match("^%s*$") then
            error("search query cannot be empty")
        end
        local count = tonumber(args.count) or 5
        if count > MAX_RESULTS then count = MAX_RESULTS end
        if count < 1 then count = 1 end
        local api_key = genesis.config.env("BRAVE_API_KEY")
        if api_key and #api_key > 0 then
            return search_brave(args.query, count, api_key)
        else
            return search_ddg(args.query, count)
        end
    end,
})
