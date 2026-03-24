-- Bundled Home Assistant tools: list entities, get state, list services, call service
-- Primitives: http (with allow_private), json, config

local BLOCKED_DOMAINS = {
    "shell_command", "command_line", "python_script",
    "pyscript", "hassio", "rest_command",
}

local MAX_ENTITIES = 100

local function is_blocked_domain(domain)
    for _, blocked in ipairs(BLOCKED_DOMAINS) do
        if domain == blocked then return true end
    end
    return false
end

local function is_valid_identifier(s)
    return s and #s > 0 and s:find("[^%l%d_]") == nil
end

local function validate_entity_id(entity_id)
    local domain, object_id = entity_id:match("^([^%.]+)%.(.+)$")
    if not domain or not object_id then return false end
    return is_valid_identifier(domain) and is_valid_identifier(object_id)
end

local function ha_config()
    local url = genesis.config.env("HASS_URL")
        or genesis.config.env("HOMEASSISTANT_URL")
        or "http://homeassistant.local:8123"
    local token = genesis.config.env("HASS_TOKEN")
        or genesis.config.env("HOMEASSISTANT_LONG_LIVED_TOKEN")
        or ""
    url = url:gsub("/$", "")
    return url, token
end

local function ha_get(path)
    local url, token = ha_config()
    if #token == 0 then
        error("HASS_TOKEN environment variable is not set")
    end
    local resp = genesis.http.request(url .. path, {
        method = "GET",
        headers = {
            ["Authorization"] = "Bearer " .. token,
            ["Content-Type"] = "application/json",
        },
        allow_private = true,
    })
    if resp.status < 200 or resp.status >= 300 then
        error("HA API returned " .. resp.status .. ": " .. resp.body)
    end
    return genesis.json.decode(resp.body)
end

local function ha_post(path, payload)
    local url, token = ha_config()
    if #token == 0 then
        error("HASS_TOKEN environment variable is not set")
    end
    local resp = genesis.http.request(url .. path, {
        method = "POST",
        headers = {
            ["Authorization"] = "Bearer " .. token,
            ["Content-Type"] = "application/json",
        },
        body = genesis.json.encode(payload),
        allow_private = true,
    })
    if resp.status < 200 or resp.status >= 300 then
        error("HA API returned " .. resp.status .. ": " .. resp.body)
    end
    return genesis.json.decode(resp.body)
end

genesis.register_tool({
    name = "ha_list_entities",
    description = "List Home Assistant entities. Optionally filter by domain (light, switch, climate, sensor, etc.) or by area name (living room, kitchen, etc.).",
    parameters = {
        domain = {
            type = "string",
            description = "Entity domain filter (e.g. 'light', 'switch', 'climate', 'sensor', 'binary_sensor', 'cover', 'fan', 'media_player').",
        },
        area = {
            type = "string",
            description = "Area/room name filter (e.g. 'living room', 'kitchen'). Matches against friendly names.",
        },
    },
    run = function(args)
        local states = ha_get("/api/states")
        if type(states) ~= "table" then
            error("unexpected HA response format")
        end
        local filtered = {}
        local domain_prefix = args.domain and (args.domain .. ".") or nil
        local area_lower = args.area and args.area:lower() or nil
        for _, s in ipairs(states) do
            if #filtered >= MAX_ENTITIES then break end
            local eid = s.entity_id or ""
            local domain_ok = true
            local area_ok = true
            if domain_prefix then
                domain_ok = eid:sub(1, #domain_prefix) == domain_prefix
            end
            if area_lower then
                local attrs = s.attributes or {}
                local fname = (attrs.friendly_name or ""):lower()
                local area_attr = (attrs.area or ""):lower()
                area_ok = fname:find(area_lower, 1, true) ~= nil
                    or area_attr:find(area_lower, 1, true) ~= nil
            end
            if domain_ok and area_ok then
                table.insert(filtered, {
                    entity_id = s.entity_id,
                    state = s.state,
                    friendly_name = s.attributes and s.attributes.friendly_name,
                })
            end
        end
        return genesis.json.encode({
            count = #filtered,
            entities = filtered,
        })
    end,
})

genesis.register_tool({
    name = "ha_get_state",
    description = "Get the detailed state of a single Home Assistant entity, including all attributes (brightness, color, temperature setpoint, sensor readings, etc.).",
    parameters = {
        entity_id = {
            type = "string",
            description = "The entity ID to query (e.g. 'light.living_room', 'climate.thermostat', 'sensor.temperature').",
            required = true,
        },
    },
    run = function(args)
        if not args.entity_id or #args.entity_id == 0 then
            error("entity_id argument is required")
        end
        if not validate_entity_id(args.entity_id) then
            error("invalid entity_id format: " .. args.entity_id)
        end
        local data = ha_get("/api/states/" .. args.entity_id)
        return genesis.json.encode({
            entity_id = data.entity_id,
            state = data.state,
            attributes = data.attributes,
            last_changed = data.last_changed,
            last_updated = data.last_updated,
        })
    end,
})

genesis.register_tool({
    name = "ha_list_services",
    description = "List available Home Assistant services (actions) for device control. Shows what actions can be performed on each device type and their parameters.",
    parameters = {
        domain = {
            type = "string",
            description = "Filter by domain (e.g. 'light', 'climate', 'switch'). Omit to list all.",
        },
    },
    run = function(args)
        local services = ha_get("/api/services")
        if type(services) ~= "table" then
            error("unexpected HA response format")
        end
        local domains = {}
        for _, s in ipairs(services) do
            local d = s.domain or ""
            local include = not args.domain or d == args.domain
            if include then
                local compact_services = {}
                if type(s.services) == "table" then
                    for name, info in pairs(s.services) do
                        local entry = { description = info.description or "" }
                        if type(info.fields) == "table" then
                            local fields = {}
                            for k, v in pairs(info.fields) do
                                if type(v) == "table" and v.description then
                                    fields[k] = v.description
                                end
                            end
                            if next(fields) then
                                entry.fields = fields
                            end
                        end
                        compact_services[name] = entry
                    end
                end
                table.insert(domains, {
                    domain = d,
                    services = compact_services,
                })
            end
        end
        return genesis.json.encode({
            count = #domains,
            domains = domains,
        })
    end,
})

genesis.register_tool({
    name = "ha_call_service",
    description = "Call a Home Assistant service to control a device. Use ha_list_services to discover available services and parameters.",
    approval = "always",
    parameters = {
        domain = {
            type = "string",
            description = "Service domain (e.g. 'light', 'switch', 'climate', 'cover', 'media_player', 'fan', 'scene', 'script').",
            required = true,
        },
        service = {
            type = "string",
            description = "Service name (e.g. 'turn_on', 'turn_off', 'toggle', 'set_temperature').",
            required = true,
        },
        entity_id = {
            type = "string",
            description = "Target entity ID (e.g. 'light.living_room'). Some services may not need this.",
        },
        data = {
            type = "string",
            description = "Additional service data as JSON string. Examples: '{\"brightness\": 255}' for lights, '{\"temperature\": 22}' for climate.",
        },
    },
    run = function(args)
        if not args.domain or #args.domain == 0 then
            error("domain argument is required")
        end
        if not args.service or #args.service == 0 then
            error("service argument is required")
        end
        if not is_valid_identifier(args.domain) then
            error("invalid domain format: " .. args.domain)
        end
        if not is_valid_identifier(args.service) then
            error("invalid service format: " .. args.service)
        end
        if is_blocked_domain(args.domain) then
            error("service domain '" .. args.domain .. "' is blocked for security. Blocked domains: "
                .. table.concat(BLOCKED_DOMAINS, ", "))
        end
        if args.entity_id and #args.entity_id > 0 then
            if not validate_entity_id(args.entity_id) then
                error("invalid entity_id format: " .. args.entity_id)
            end
        end
        local payload = {}
        if args.data and #args.data > 0 then
            local ok, parsed = pcall(genesis.json.decode, args.data)
            if ok and type(parsed) == "table" then
                payload = parsed
            end
        end
        if args.entity_id and #args.entity_id > 0 then
            payload.entity_id = args.entity_id
        end
        local result = ha_post("/api/services/" .. args.domain .. "/" .. args.service, payload)
        local affected = {}
        if type(result) == "table" then
            for _, s in ipairs(result) do
                if type(s) == "table" then
                    table.insert(affected, {
                        entity_id = s.entity_id,
                        state = s.state,
                    })
                end
            end
        end
        return genesis.json.encode({
            success = true,
            service = args.domain .. "." .. args.service,
            affected_entities = affected,
        })
    end,
})
