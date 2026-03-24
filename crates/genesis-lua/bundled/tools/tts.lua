-- Bundled text-to-speech tool: edge-tts CLI wrapper
-- Primitives: process, fs

local DEFAULT_VOICE = "en-US-AriaNeural"

genesis.register_tool({
    name = "text_to_speech",
    description = "Generates speech audio from text using edge-tts and writes an MP3 file.",
    approval = "destructive",
    parameters = {
        text = {
            type = "string",
            description = "Text to synthesize into speech.",
            required = true,
        },
        output_path = {
            type = "string",
            description = "Path to the output MP3 file.",
            required = true,
        },
        voice = {
            type = "string",
            description = "Optional voice name (default: en-US-AriaNeural).",
        },
        rate = {
            type = "string",
            description = "Optional speech rate adjustment like '+20%' or '-10%'.",
        },
    },
    run = function(args)
        if not args.text or #args.text == 0 then
            error("text argument is required")
        end
        if not args.output_path or #args.output_path == 0 then
            error("output_path argument is required")
        end

        local voice = args.voice or DEFAULT_VOICE
        local output_path = args.output_path

        -- Create parent directory if needed
        local parent = output_path:match("^(.+)/[^/]+$")
        if parent and #parent > 0 then
            genesis.fs.mkdir(parent)
        end

        -- Build command with shell-quoted arguments to prevent injection
        local cmd = string.format(
            "edge-tts --text %s --voice %s --write-media %s",
            string.format("%q", args.text),
            string.format("%q", voice),
            string.format("%q", output_path)
        )
        if args.rate and #args.rate > 0 then
            cmd = cmd .. " --rate " .. string.format("%q", args.rate)
        end

        local result = genesis.process.exec(cmd)

        if result.exit_code ~= 0 then
            local stderr = result.stderr:match("^%s*(.-)%s*$") or ""
            if #stderr == 0 then
                error("edge-tts exited with status " .. result.exit_code)
            else
                error("edge-tts failed: " .. stderr)
            end
        end

        return {
            content = "audio written to " .. output_path,
            metadata = {
                tool = "text_to_speech",
                voice = voice,
                output_path = output_path,
            },
        }
    end,
})
