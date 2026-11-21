--!strict
-- 引擎注入的全域表: cast 成 any 讓型別分析器別再報未知全域, 執行時無效果
local theraohe = (_G :: any).theraohe

-- 加密探針: 逐環驗證 World -> Level -> Actors TArray 鏈路, 把每環狀態同時畫在左上角與 print;
-- 決賽圈前後對照哪一環從 OK 變 FAIL/亂數, 即加密入口; 無斷點, 純量測
--
-- 配置見文件尾部全域變量, Luau 頁可熱改 (.toon 持久化)

local module_candidates = { "libue4.so", "UE4", "Unreal", "Game", "PUBGM" }

local GWORLD_PATTERN = '[[["%s"+0xFC70A70]+0x810]+0x78]'
local CTRL_PATTERN = '[[[[[["%s"+0xFC70A70]+0x810]+0x78]+0x38]+0x78]+0x30]'
local ONSELF_PATTERN = '[[[[[[["%s"+0xFC70A70]+0x810]+0x78]+0x38]+0x78]+0x30]+0x29F8]'

local PLAYER_MARK_OFF = 0x2cd8
local PLAYER_MARK = 479.5

-- 配置: 多少 tick 重測一次並 print; 畫面每幀重繪 (成本可忽略)
DetectorInterval = 60 :: number
-- 每次抽樣的 actor 數上限 (均勻分布全陣列)
SampleActors = 128 :: number

local HEAP_LO = 0x10000000
local HEAP_HI = 0x10000000000
local NUM_MAX = 0x7530

local function resolve_first(pattern: string): number?
    for _, pat in ipairs(module_candidates) do
        local p = theraohe.resolve_chain(string.format(pattern, pat))
        if p then
            return p
        end
    end
    return nil
end

type Line = { text: string, r: number, g: number, b: number }
local lines: { Line } = {}

local function add(text: string, r: number?, g: number?, b: number?)
    lines[#lines + 1] = { text = text, r = r or 255, g = g or 255, b = b or 255 }
end
local GREY = { 205, 205, 205 }
local OK = { 0, 255, 120 }
local BAD = { 255, 90, 90 }
local WARN = { 255, 255, 0 }

local function tri(name: string, ok: boolean, detail: string?)
    local c = ok and OK or BAD
    add(name .. (ok and ": OK" or ": FAIL") .. (detail and ("  " .. detail) or ""), c[1], c[2], c[3])
end

local function probe()
    table.clear(lines)
    local base = theraohe.module_base()
    tri("driver", base ~= nil and base ~= 0, base and string.format("%x", base) or nil)

    local gworld = resolve_first(GWORLD_PATTERN)
    tri("gworld", gworld ~= nil, gworld and string.format("%x", gworld) or nil)

    local level = gworld and theraohe.read_u64(gworld + 0x30) or nil
    local level_ok = level ~= nil and level >= HEAP_LO and level < HEAP_HI
    tri("level", level_ok, level and string.format("%x", level) or nil)

    local arr, num = nil, nil
    if level_ok then
        arr = theraohe.read_u64(level + 0xa0)
        num = theraohe.read_u32(level + 0xa8)
    end
    local arr_ok = arr ~= nil and arr % 8 == 0 and arr >= HEAP_LO and arr < HEAP_HI
        and num ~= nil and num > 0 and num <= NUM_MAX
        and (arr + num * 8) <= HEAP_HI
    tri("tarray hdr", arr_ok, arr and string.format("arr=%x num=%s", arr, tostring(num)) or nil)

    -- 自己 actor 與 root 的指針 (決賽圈翻紅後拿來掃新陣列位置; 一定要印 hex)
    local onself_ptr = resolve_first(ONSELF_PATTERN)
    if onself_ptr then
        add(string.format("onself %x", onself_ptr), GREY[1], GREY[2], GREY[3])
        local root = theraohe.read_u64(onself_ptr + 0x208)
        if root and root ~= 0 then
            add(string.format("own root %x", root), GREY[1], GREY[2], GREY[3])
        end
    else
        add("onself: nil", BAD[1], BAD[2], BAD[3])
    end

    -- 均勻抽樣: 存活率 / marker 命中 / onself 出現位置
    if arr_ok and arr and num then
        local alive, marked, onself_idx = 0, 0, -1
        local sample = math.min(num, SampleActors)
        local stride = math.max(1, math.floor(num / sample))
        local onself = resolve_first(ONSELF_PATTERN)
        local first_fail: number? = nil
        local iters = 0
        for i = 0, num - 1, stride do
            iters += 1
            local actor = theraohe.read_u64(arr + i * 8)
            if actor and actor >= HEAP_LO and actor < HEAP_HI and actor % 8 == 0 then
                alive += 1
                local m = theraohe.read_f32(actor + PLAYER_MARK_OFF)
                if m == PLAYER_MARK then
                    marked += 1
                end
                if onself and actor == onself then
                    onself_idx = i
                end
            elseif first_fail == nil then
                first_fail = actor
            end
        end
        -- 真實陣列本來就有空槽/已釋放指針, 不能要求全部存活; 只抓整體性壞掉的情況
        local c = alive == iters and OK or (alive >= iters * 0.5 and WARN or BAD)
        add(string.format("sample alive %d/%d marked=%d%s", alive, iters, marked,
            first_fail and string.format(" first_fail=%x", first_fail) or ""), c[1], c[2], c[3])
        if onself_idx >= 0 then
            add(string.format("onself in arr idx=%d", onself_idx), OK[1], OK[2], OK[3])
        else
            add("onself in arr: 抽樣未命中 (可能落在未抽樣區或加密)", WARN[1], WARN[2], WARN[3])
        end
    end

    -- 自己 root vs 相機: 第三人稱橫距無參考意義, dz (root 高度差) 才拿來當錨點
    local onself = resolve_first(ONSELF_PATTERN)
    local ctrl = resolve_first(CTRL_PATTERN)
    local pcm = ctrl and theraohe.read_u64(ctrl + 0x548) or nil
    local root = onself and theraohe.read_u64(onself + 0x208) or nil
    if pcm and pcm ~= 0 and root and root ~= 0 then
        local px = theraohe.read_f32(pcm + 0x530)
        local py = theraohe.read_f32(pcm + 0x534)
        local pz = theraohe.read_f32(pcm + 0x538)
        local rx = theraohe.read_f32(root + 0x220)
        local ry = theraohe.read_f32(root + 0x224)
        local rz = theraohe.read_f32(root + 0x228)
        if px and py and pz and rx and ry and rz then
            -- PUBGM 第三人稱相機在角色後上方: 橫距無參考意義只報告; 錨點用 cam.z - root.z (TPP 相機偏上)
            local dh = math.sqrt((rx - px) * (rx - px) + (ry - py) * (ry - py))
            local dz = pz - rz
            add(string.format("cam dh=%.1f", dh), GREY[1], GREY[2], GREY[3])
            if dz >= 30.0 and dz <= 300.0 then
                add(string.format("cam-root dz=%.1f: OK", dz), OK[1], OK[2], OK[3])
            else
                add(string.format("cam-root dz=%.1f: FAIL", dz), BAD[1], BAD[2], BAD[3])
            end
        else
            add("own-root/cam: 讀取失敗", BAD[1], BAD[2], BAD[3])
        end
    else
        add("own-root/cam: 鏈路失敗", BAD[1], BAD[2], BAD[3])
    end

    print("=== RodinEncryptionDetector probe ===")
    for _, l in ipairs(lines) do
        print(l.text)
    end
end

local function draw()
    local y = 40
    for _, l in ipairs(lines) do
        theraohe.draw_text(20, y, 0, 0, l.text, l.r, l.g, l.b, nil, 1)
        y = y + 22
    end
end

local ticks: number = 0

function Tick(): ()
    local ok, err = pcall(function()
        ticks = ticks + 1
        local interval = tonumber(DetectorInterval) or 60
        if ticks % interval == 1 or #lines == 0 then
            probe()
        end
        draw()
    end)
    if not ok then
        print("[detector] ERROR: " .. tostring(err))
    end
end
