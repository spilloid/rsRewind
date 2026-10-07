// Rounded, bordered quads, optionally showing a texture (a screenshot thumbnail) letterboxed inside,
// with an optional soft outer glow. Coordinates are logical pixels relative to the widget; the
// render pass viewport already covers exactly the widget's bounds.

struct Globals {
    size: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var samp: sampler;
@group(1) @binding(0) var tex: texture_2d<f32>;

struct Instance {
    @location(0) rect: vec4<f32>,   // x, y, w, h
    @location(1) image: vec4<f32>,  // x, y, w, h of the picture; w == 0 means none
    @location(2) fill: vec4<f32>,   // straight-alpha colour behind / instead of the picture
    @location(3) border: vec4<f32>, // straight-alpha border (and glow) colour
    @location(4) params: vec4<f32>, // corner radius, border width, opacity, glow radius
};

struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) px: vec2<f32>,
    @location(1) @interpolate(flat) rect: vec4<f32>,
    @location(2) @interpolate(flat) image: vec4<f32>,
    @location(3) @interpolate(flat) fill: vec4<f32>,
    @location(4) @interpolate(flat) border: vec4<f32>,
    @location(5) @interpolate(flat) params: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32, instance: Instance) -> Out {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
    );
    let corner = corners[index];
    // Grow the quad so the glow and the anti-aliased edge have room.
    let margin = instance.params.w + 2.0;
    let px = instance.rect.xy - vec2<f32>(margin) + corner * (instance.rect.zw + vec2<f32>(2.0 * margin));
    let size = max(globals.size, vec2<f32>(1.0));
    var out: Out;
    out.position = vec4<f32>(px.x / size.x * 2.0 - 1.0, 1.0 - px.y / size.y * 2.0, 0.0, 1.0);
    out.px = px;
    out.rect = instance.rect;
    out.image = instance.image;
    out.fill = instance.fill;
    out.border = instance.border;
    out.params = instance.params;
    return out;
}

fn rounded_box(p: vec2<f32>, half: vec2<f32>, radius: f32) -> f32 {
    let q = abs(p) - half + vec2<f32>(radius);
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - radius;
}

@fragment
fn fs_main(in: Out) -> @location(0) vec4<f32> {
    let half = in.rect.zw * 0.5;
    let center = in.rect.xy + half;
    let radius = min(in.params.x, min(half.x, half.y));
    let d = rounded_box(in.px - center, half, radius);
    let aa = max(fwidth(d), 0.35);
    let shape = clamp(0.5 - d / aa, 0.0, 1.0);

    // Sampled unconditionally: texture sampling must stay in uniform control flow.
    let uv = (in.px - in.image.xy) / max(in.image.zw, vec2<f32>(0.001));
    let sampled = textureSample(tex, samp, clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0)));
    let has_image = in.image.z > 0.0 && all(uv >= vec2<f32>(0.0)) && all(uv <= vec2<f32>(1.0));
    let content = select(in.fill, vec4<f32>(sampled.rgb, 1.0), has_image);

    let border_width = in.params.y;
    let inner = select(1.0, clamp(0.5 - (d + border_width) / aa, 0.0, 1.0), border_width > 0.0);
    let color = mix(in.border, content, inner);

    let opacity = in.params.z;
    var alpha = color.a * shape * opacity;
    var rgb = color.rgb * alpha;

    let glow = in.params.w;
    if glow > 0.0 {
        let outside = max(d, 0.0);
        let g = in.border.a * 0.6 * exp(-outside / (glow * 0.4)) * (1.0 - shape) * opacity;
        rgb = rgb + in.border.rgb * g;
        alpha = alpha + g;
    }
    return vec4<f32>(rgb, min(alpha, 1.0));
}
