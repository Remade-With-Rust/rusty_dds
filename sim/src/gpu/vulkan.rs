//! The Vulkan viewport.
//!
//! The same conventional renderer shape as the D3D11 one, expressed in Vulkan:
//! FIFO present mode (the vsync that `Present(1)` gives on D3D11), push
//! constants for the MVP, staged `vkCmdCopyBufferToImage` for streamed mips, and
//! a per-texture image view whose `baseMipLevel` is the finest resident mip —
//! Vulkan's equivalent of `SetResourceMinLOD`, so sampling never shows detail
//! the streamer has not delivered.
//!
//! One frame in flight, waited on a fence. That is slower than a deep pipeline
//! and entirely deliberate: it makes descriptor updates and resource destruction
//! trivially safe, and the pane is vsync-paced anyway. The same rails as D3D11
//! apply — nothing is freed on eviction, and `trim` defers destruction to after
//! the next fence wait.

use std::collections::HashMap;
use std::ffi::CStr;

use ash::vk;

use crate::gpu::scene::{Quad, View};
use crate::gpu::window::Window;
use crate::gpu::{Api, GpuLimits, GpuTimings, Viewport, ViewportConfig};
use crate::provider::{SimError, SimResult, SubId, SubresourceBytes, TextureDesc};

const SPIRV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/quad.spv"));
const VS_ENTRY: &CStr = c"vs_main";
const FS_ENTRY: &CStr = c"fs_main";

#[repr(C)]
#[derive(Clone, Copy)]
struct Vertex {
    pos: [f32; 3],
    uv: [f32; 2],
}

const QUAD: [Vertex; 6] = [
    Vertex { pos: [-0.5, -0.5, 0.0], uv: [0.0, 1.0] },
    Vertex { pos: [-0.5, 0.5, 0.0], uv: [0.0, 0.0] },
    Vertex { pos: [0.5, 0.5, 0.0], uv: [1.0, 0.0] },
    Vertex { pos: [-0.5, -0.5, 0.0], uv: [0.0, 1.0] },
    Vertex { pos: [0.5, 0.5, 0.0], uv: [1.0, 0.0] },
    Vertex { pos: [0.5, -0.5, 0.0], uv: [1.0, 1.0] },
];

fn vk_format(name: &str) -> vk::Format {
    match name {
        "BC1_UNorm" => vk::Format::BC1_RGBA_UNORM_BLOCK,
        "BC1_UNorm_sRGB" => vk::Format::BC1_RGBA_SRGB_BLOCK,
        "BC2_UNorm" => vk::Format::BC2_UNORM_BLOCK,
        "BC3_UNorm" => vk::Format::BC3_UNORM_BLOCK,
        "BC3_UNorm_sRGB" => vk::Format::BC3_SRGB_BLOCK,
        "BC4_UNorm" => vk::Format::BC4_UNORM_BLOCK,
        "BC4_SNorm" => vk::Format::BC4_SNORM_BLOCK,
        "BC5_UNorm" => vk::Format::BC5_UNORM_BLOCK,
        "BC5_SNorm" => vk::Format::BC5_SNORM_BLOCK,
        "BC6H_UF16" => vk::Format::BC6H_UFLOAT_BLOCK,
        "BC7_UNorm" => vk::Format::BC7_UNORM_BLOCK,
        "BC7_UNorm_sRGB" => vk::Format::BC7_SRGB_BLOCK,
        "B8G8R8A8_UNorm" => vk::Format::B8G8R8A8_UNORM,
        _ => vk::Format::R8G8B8A8_UNORM,
    }
}

struct VkTexture {
    image: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
    set: vk::DescriptorSet,
    format: vk::Format,
    mips: u32,
    /// `baseMipLevel` the current view was built with.
    view_base: u32,
    /// Mips that have been copied into and are in SHADER_READ_ONLY_OPTIMAL.
    uploaded: u32,
    bytes: u64,
    last_used: u64,
    live: bool,
}

pub struct VulkanViewport {
    window: Window,
    _entry: ash::Entry,
    instance: ash::Instance,
    surface_fn: ash::khr::surface::Instance,
    surface: vk::SurfaceKHR,
    device: ash::Device,
    phys: vk::PhysicalDevice,
    mem_props: vk::PhysicalDeviceMemoryProperties,
    queue: vk::Queue,
    queue_family: u32,

    swapchain_fn: ash::khr::swapchain::Device,
    swapchain: vk::SwapchainKHR,
    extent: vk::Extent2D,
    views: Vec<vk::ImageView>,
    framebuffers: Vec<vk::Framebuffer>,
    render_pass: vk::RenderPass,

    pipeline_layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    set_layout: vk::DescriptorSetLayout,
    desc_pool: vk::DescriptorPool,
    sampler: vk::Sampler,

    cmd_pool: vk::CommandPool,
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    sem_acquire: vk::Semaphore,
    sem_render: vk::Semaphore,

    vbuf: vk::Buffer,
    vmem: vk::DeviceMemory,
    staging: vk::Buffer,
    staging_mem: vk::DeviceMemory,
    staging_size: u64,
    staging_ptr: *mut u8,
    /// Offset within the CURRENT region.
    staging_at: u64,
    /// Base of the current region. The staging buffer is double-buffered
    /// because uploads are memcpy'd in *before* `frame()` waits on the fence:
    /// with a single region the CPU would overwrite bytes the GPU is still
    /// reading for the previous frame's copies. That race produced an
    /// intermittent `ERROR_DEVICE_LOST` under four-pane contention.
    staging_base: u64,
    staging_region: u64,

    query_pool: vk::QueryPool,
    timestamp_period: f32,
    have_pending_query: bool,

    /// Uploads recorded into the current frame's command buffer.
    barriers_pre: Vec<vk::ImageMemoryBarrier<'static>>,
    copies: Vec<(vk::Image, vk::BufferImageCopy)>,
    barriers_post: Vec<vk::ImageMemoryBarrier<'static>>,

    textures: HashMap<u32, VkTexture>,
    /// Destruction deferred to after the next fence wait — never mid-frame.
    graveyard: Vec<VkTexture>,
    /// Image views replaced by `set_min_lod`. They cannot be destroyed on the
    /// spot: the previous frame's command buffer may still reference them
    /// through a descriptor set.
    dead_views: Vec<vk::ImageView>,
    /// GPU milliseconds read from the previous frame's timestamps in
    /// `begin_frame`, reported by the `frame` that follows it.
    last_gpu_ms: f64,
    gpu_bytes: u64,
    frame_counter: u64,
}

fn err<T: std::fmt::Debug>(what: &str, e: T) -> SimError {
    SimError(format!("vulkan: {what}: {e:?}"))
}

impl VulkanViewport {
    fn find_memory(&self, bits: u32, flags: vk::MemoryPropertyFlags) -> SimResult<u32> {
        for i in 0..self.mem_props.memory_type_count {
            if bits & (1 << i) != 0
                && self.mem_props.memory_types[i as usize]
                    .property_flags
                    .contains(flags)
            {
                return Ok(i);
            }
        }
        Err(SimError("vulkan: no suitable memory type".into()))
    }

    pub fn new(cfg: &ViewportConfig) -> SimResult<VulkanViewport> {
        let window = Window::new(&cfg.title, cfg.x, cfg.y, cfg.width, cfg.height)
            .map_err(|e| SimError(format!("window: {e}")))?;

        // SAFETY: the whole of Vulkan bring-up. Every handle below is created by
        // the driver, checked for error, and owned by the returned struct, which
        // destroys them in reverse order in `Drop`.
        unsafe {
            let entry = ash::Entry::load().map_err(|e| err("loading vulkan-1", e))?;

            let app = vk::ApplicationInfo::default()
                .application_name(c"rusty_dds_sim")
                .api_version(vk::make_api_version(0, 1, 1, 0));
            let exts = [
                ash::khr::surface::NAME.as_ptr(),
                ash::khr::win32_surface::NAME.as_ptr(),
            ];
            let instance = entry
                .create_instance(
                    &vk::InstanceCreateInfo::default()
                        .application_info(&app)
                        .enabled_extension_names(&exts),
                    None,
                )
                .map_err(|e| err("create_instance", e))?;

            let surface_fn = ash::khr::surface::Instance::new(&entry, &instance);
            let win32_fn = ash::khr::win32_surface::Instance::new(&entry, &instance);
            let surface = win32_fn
                .create_win32_surface(
                    &vk::Win32SurfaceCreateInfoKHR::default()
                        .hwnd(window.hwnd.0 as isize)
                        .hinstance(
                            windows::Win32::System::LibraryLoader::GetModuleHandleW(None)
                                .map(|h| h.0 as isize)
                                .unwrap_or(0),
                        ),
                    None,
                )
                .map_err(|e| err("create_win32_surface", e))?;

            // Pick the first device with a queue that can both render and present.
            let phys_devices = instance
                .enumerate_physical_devices()
                .map_err(|e| err("enumerate_physical_devices", e))?;
            let mut chosen = None;
            for pd in phys_devices {
                let families = instance.get_physical_device_queue_family_properties(pd);
                for (i, f) in families.iter().enumerate() {
                    let graphics = f.queue_flags.contains(vk::QueueFlags::GRAPHICS);
                    let present = surface_fn
                        .get_physical_device_surface_support(pd, i as u32, surface)
                        .unwrap_or(false);
                    if graphics && present {
                        chosen = Some((pd, i as u32));
                        break;
                    }
                }
                if chosen.is_some() {
                    break;
                }
            }
            let (phys, queue_family) =
                chosen.ok_or_else(|| SimError("vulkan: no graphics+present queue".into()))?;

            let props = instance.get_physical_device_properties(phys);
            let timestamp_period = props.limits.timestamp_period;
            let mem_props = instance.get_physical_device_memory_properties(phys);

            let prio = [1.0f32];
            let qinfo = [vk::DeviceQueueCreateInfo::default()
                .queue_family_index(queue_family)
                .queue_priorities(&prio)];
            let dev_exts = [ash::khr::swapchain::NAME.as_ptr()];
            // BCn sampling is core Vulkan 1.0 but gated on a feature bit.
            let features = vk::PhysicalDeviceFeatures::default()
                .texture_compression_bc(true)
                .sampler_anisotropy(true);
            let device = instance
                .create_device(
                    phys,
                    &vk::DeviceCreateInfo::default()
                        .queue_create_infos(&qinfo)
                        .enabled_extension_names(&dev_exts)
                        .enabled_features(&features),
                    None,
                )
                .map_err(|e| err("create_device", e))?;
            let queue = device.get_device_queue(queue_family, 0);
            let swapchain_fn = ash::khr::swapchain::Device::new(&instance, &device);

            let mut vp = VulkanViewport {
                window,
                _entry: entry,
                instance,
                surface_fn,
                surface,
                device,
                phys,
                mem_props,
                queue,
                queue_family,
                swapchain_fn,
                swapchain: vk::SwapchainKHR::null(),
                extent: vk::Extent2D {
                    width: cfg.width,
                    height: cfg.height,
                },
                views: Vec::new(),
                framebuffers: Vec::new(),
                render_pass: vk::RenderPass::null(),
                pipeline_layout: vk::PipelineLayout::null(),
                pipeline: vk::Pipeline::null(),
                set_layout: vk::DescriptorSetLayout::null(),
                desc_pool: vk::DescriptorPool::null(),
                sampler: vk::Sampler::null(),
                cmd_pool: vk::CommandPool::null(),
                cmd: vk::CommandBuffer::null(),
                fence: vk::Fence::null(),
                sem_acquire: vk::Semaphore::null(),
                sem_render: vk::Semaphore::null(),
                vbuf: vk::Buffer::null(),
                vmem: vk::DeviceMemory::null(),
                staging: vk::Buffer::null(),
                staging_mem: vk::DeviceMemory::null(),
                staging_size: 0,
                staging_ptr: std::ptr::null_mut(),
                staging_at: 0,
                staging_base: 0,
                staging_region: 0,
                query_pool: vk::QueryPool::null(),
                timestamp_period,
                have_pending_query: false,
                barriers_pre: Vec::new(),
                copies: Vec::new(),
                barriers_post: Vec::new(),
                textures: HashMap::new(),
                graveyard: Vec::new(),
                dead_views: Vec::new(),
                last_gpu_ms: f64::NAN,
                gpu_bytes: 0,
                frame_counter: 0,
            };

            vp.create_swapchain()?;
            vp.create_pipeline()?;
            vp.create_frame_resources()?;
            Ok(vp)
        }
    }

    unsafe fn create_swapchain(&mut self) -> SimResult<()> {
        let caps = unsafe {
            self.surface_fn
                .get_physical_device_surface_capabilities(self.phys, self.surface)
        }
        .map_err(|e| err("surface_capabilities", e))?;
        let formats = unsafe {
            self.surface_fn
                .get_physical_device_surface_formats(self.phys, self.surface)
        }
        .map_err(|e| err("surface_formats", e))?;
        let format = formats
            .iter()
            .find(|f| f.format == vk::Format::B8G8R8A8_UNORM)
            .copied()
            .unwrap_or(formats[0]);

        self.extent = if caps.current_extent.width != u32::MAX {
            caps.current_extent
        } else {
            self.extent
        };
        let count = (caps.min_image_count + 1).min(if caps.max_image_count == 0 {
            u32::MAX
        } else {
            caps.max_image_count
        });

        self.swapchain = unsafe {
            self.swapchain_fn.create_swapchain(
                &vk::SwapchainCreateInfoKHR::default()
                    .surface(self.surface)
                    .min_image_count(count)
                    .image_format(format.format)
                    .image_color_space(format.color_space)
                    .image_extent(self.extent)
                    .image_array_layers(1)
                    .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
                    .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
                    .pre_transform(caps.current_transform)
                    .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
                    // FIFO is vsync, and is the only mode guaranteed present.
                    // It is the deliberate counterpart of D3D11's Present(1).
                    .present_mode(vk::PresentModeKHR::FIFO)
                    .clipped(true),
                None,
            )
        }
        .map_err(|e| err("create_swapchain", e))?;

        let images = unsafe { self.swapchain_fn.get_swapchain_images(self.swapchain) }
            .map_err(|e| err("get_swapchain_images", e))?;

        // Render pass: one colour attachment, cleared, presented.
        let attachment = [vk::AttachmentDescription::default()
            .format(format.format)
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::STORE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .final_layout(vk::ImageLayout::PRESENT_SRC_KHR)];
        let color_ref = [vk::AttachmentReference::default()
            .attachment(0)
            .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)];
        let subpass = [vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(&color_ref)];
        let dep = [vk::SubpassDependency::default()
            .src_subpass(vk::SUBPASS_EXTERNAL)
            .dst_subpass(0)
            .src_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
            .dst_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
            .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)];
        self.render_pass = unsafe {
            self.device.create_render_pass(
                &vk::RenderPassCreateInfo::default()
                    .attachments(&attachment)
                    .subpasses(&subpass)
                    .dependencies(&dep),
                None,
            )
        }
        .map_err(|e| err("create_render_pass", e))?;

        for image in images {
            let view = unsafe {
                self.device.create_image_view(
                    &vk::ImageViewCreateInfo::default()
                        .image(image)
                        .view_type(vk::ImageViewType::TYPE_2D)
                        .format(format.format)
                        .subresource_range(
                            vk::ImageSubresourceRange::default()
                                .aspect_mask(vk::ImageAspectFlags::COLOR)
                                .level_count(1)
                                .layer_count(1),
                        ),
                    None,
                )
            }
            .map_err(|e| err("create_image_view", e))?;
            let attach = [view];
            let fb = unsafe {
                self.device.create_framebuffer(
                    &vk::FramebufferCreateInfo::default()
                        .render_pass(self.render_pass)
                        .attachments(&attach)
                        .width(self.extent.width)
                        .height(self.extent.height)
                        .layers(1),
                    None,
                )
            }
            .map_err(|e| err("create_framebuffer", e))?;
            self.views.push(view);
            self.framebuffers.push(fb);
        }
        Ok(())
    }

    unsafe fn create_pipeline(&mut self) -> SimResult<()> {
        let words: Vec<u32> = SPIRV
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let module = unsafe {
            self.device
                .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words), None)
        }
        .map_err(|e| err("create_shader_module", e))?;

        // Separate image and sampler bindings, matching the WGSL. Keeping them
        // apart lets the per-texture view carry `baseMipLevel` while one sampler
        // serves every draw.
        let bindings = [
            vk::DescriptorSetLayoutBinding::default()
                .binding(0)
                .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT),
            vk::DescriptorSetLayoutBinding::default()
                .binding(1)
                .descriptor_type(vk::DescriptorType::SAMPLER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT),
        ];
        self.set_layout = unsafe {
            self.device.create_descriptor_set_layout(
                &vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings),
                None,
            )
        }
        .map_err(|e| err("create_descriptor_set_layout", e))?;

        const MAX_SETS: u32 = 1024;
        let sizes = [
            vk::DescriptorPoolSize::default()
                .ty(vk::DescriptorType::SAMPLED_IMAGE)
                .descriptor_count(MAX_SETS),
            vk::DescriptorPoolSize::default()
                .ty(vk::DescriptorType::SAMPLER)
                .descriptor_count(MAX_SETS),
        ];
        self.desc_pool = unsafe {
            self.device.create_descriptor_pool(
                &vk::DescriptorPoolCreateInfo::default()
                    .max_sets(MAX_SETS)
                    .flags(vk::DescriptorPoolCreateFlags::FREE_DESCRIPTOR_SET)
                    .pool_sizes(&sizes),
                None,
            )
        }
        .map_err(|e| err("create_descriptor_pool", e))?;

        self.sampler = unsafe {
            self.device.create_sampler(
                &vk::SamplerCreateInfo::default()
                    .mag_filter(vk::Filter::LINEAR)
                    .min_filter(vk::Filter::LINEAR)
                    .mipmap_mode(vk::SamplerMipmapMode::LINEAR)
                    .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .anisotropy_enable(true)
                    .max_anisotropy(8.0)
                    .max_lod(vk::LOD_CLAMP_NONE),
                None,
            )
        }
        .map_err(|e| err("create_sampler", e))?;

        let set_layouts = [self.set_layout];
        let push = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX)
            .offset(0)
            .size(64)];
        self.pipeline_layout = unsafe {
            self.device.create_pipeline_layout(
                &vk::PipelineLayoutCreateInfo::default()
                    .set_layouts(&set_layouts)
                    .push_constant_ranges(&push),
                None,
            )
        }
        .map_err(|e| err("create_pipeline_layout", e))?;

        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(module)
                .name(VS_ENTRY),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(module)
                .name(FS_ENTRY),
        ];
        let bind_desc = [vk::VertexInputBindingDescription::default()
            .binding(0)
            .stride(std::mem::size_of::<Vertex>() as u32)
            .input_rate(vk::VertexInputRate::VERTEX)];
        let attr_desc = [
            vk::VertexInputAttributeDescription::default()
                .location(0)
                .binding(0)
                .format(vk::Format::R32G32B32_SFLOAT)
                .offset(0),
            vk::VertexInputAttributeDescription::default()
                .location(1)
                .binding(0)
                .format(vk::Format::R32G32_SFLOAT)
                .offset(12),
        ];
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
            .vertex_binding_descriptions(&bind_desc)
            .vertex_attribute_descriptions(&attr_desc);
        let ia = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let viewports = [vk::Viewport {
            x: 0.0,
            // Flip Y: the projection matrix is shared with D3D11, whose clip
            // space has +Y up. Flipping here rather than in the matrix keeps one
            // matrix feeding both APIs, so they cannot disagree about the scene.
            y: self.extent.height as f32,
            width: self.extent.width as f32,
            height: -(self.extent.height as f32),
            min_depth: 0.0,
            max_depth: 1.0,
        }];
        let scissors = [vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: self.extent,
        }];
        let vp_state = vk::PipelineViewportStateCreateInfo::default()
            .viewports(&viewports)
            .scissors(&scissors);
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
            .line_width(1.0);
        let msaa = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let blend_attach = [vk::PipelineColorBlendAttachmentState::default()
            .color_write_mask(vk::ColorComponentFlags::RGBA)
            .blend_enable(false)];
        let blend =
            vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attach);

        let info = [vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&ia)
            .viewport_state(&vp_state)
            .rasterization_state(&raster)
            .multisample_state(&msaa)
            .color_blend_state(&blend)
            .layout(self.pipeline_layout)
            .render_pass(self.render_pass)
            .subpass(0)];
        let pipes = unsafe {
            self.device
                .create_graphics_pipelines(vk::PipelineCache::null(), &info, None)
        }
        .map_err(|(_, e)| err("create_graphics_pipelines", e))?;
        self.pipeline = pipes[0];
        unsafe { self.device.destroy_shader_module(module, None) };
        Ok(())
    }

    unsafe fn create_buffer(
        &self,
        size: u64,
        usage: vk::BufferUsageFlags,
        props: vk::MemoryPropertyFlags,
    ) -> SimResult<(vk::Buffer, vk::DeviceMemory)> {
        let buf = unsafe {
            self.device.create_buffer(
                &vk::BufferCreateInfo::default()
                    .size(size)
                    .usage(usage)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE),
                None,
            )
        }
        .map_err(|e| err("create_buffer", e))?;
        let req = unsafe { self.device.get_buffer_memory_requirements(buf) };
        let idx = self.find_memory(req.memory_type_bits, props)?;
        let mem = unsafe {
            self.device.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(req.size)
                    .memory_type_index(idx),
                None,
            )
        }
        .map_err(|e| err("allocate_memory", e))?;
        unsafe { self.device.bind_buffer_memory(buf, mem, 0) }
            .map_err(|e| err("bind_buffer_memory", e))?;
        Ok((buf, mem))
    }

    unsafe fn create_frame_resources(&mut self) -> SimResult<()> {
        self.cmd_pool = unsafe {
            self.device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(self.queue_family)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None,
            )
        }
        .map_err(|e| err("create_command_pool", e))?;
        self.cmd = unsafe {
            self.device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(self.cmd_pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )
        }
        .map_err(|e| err("allocate_command_buffers", e))?[0];

        self.fence = unsafe {
            self.device.create_fence(
                &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                None,
            )
        }
        .map_err(|e| err("create_fence", e))?;
        self.sem_acquire = unsafe {
            self.device
                .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
        }
        .map_err(|e| err("create_semaphore", e))?;
        self.sem_render = unsafe {
            self.device
                .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
        }
        .map_err(|e| err("create_semaphore", e))?;

        // Vertex buffer: tiny and immutable, so host-visible is fine.
        let vsize = std::mem::size_of_val(&QUAD) as u64;
        let (vbuf, vmem) = unsafe {
            self.create_buffer(
                vsize,
                vk::BufferUsageFlags::VERTEX_BUFFER,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            )
        }?;
        let ptr = unsafe {
            self.device
                .map_memory(vmem, 0, vsize, vk::MemoryMapFlags::empty())
        }
        .map_err(|e| err("map_memory", e))?;
        // SAFETY: `ptr` maps at least `vsize` bytes, and QUAD is exactly that.
        unsafe { std::ptr::copy_nonoverlapping(QUAD.as_ptr() as *const u8, ptr as *mut u8, vsize as usize) };
        unsafe { self.device.unmap_memory(vmem) };
        self.vbuf = vbuf;
        self.vmem = vmem;

        // Two regions, alternating per frame; each must hold one frame's upload
        // ceiling (GpuLimits::max_upload_bytes_per_frame) with headroom.
        self.staging_region = 32 << 20;
        self.staging_size = self.staging_region * 2;
        let (sbuf, smem) = unsafe {
            self.create_buffer(
                self.staging_size,
                vk::BufferUsageFlags::TRANSFER_SRC,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            )
        }?;
        self.staging = sbuf;
        self.staging_mem = smem;
        self.staging_ptr = unsafe {
            self.device
                .map_memory(smem, 0, self.staging_size, vk::MemoryMapFlags::empty())
        }
        .map_err(|e| err("map_memory(staging)", e))? as *mut u8;

        self.query_pool = unsafe {
            self.device.create_query_pool(
                &vk::QueryPoolCreateInfo::default()
                    .query_type(vk::QueryType::TIMESTAMP)
                    .query_count(2),
                None,
            )
        }
        .map_err(|e| err("create_query_pool", e))?;
        Ok(())
    }
}

impl Viewport for VulkanViewport {
    fn api(&self) -> Api {
        Api::Vulkan
    }

    fn ensure_texture(&mut self, texture: u32, desc: &TextureDesc) -> SimResult<()> {
        if let Some(t) = self.textures.get_mut(&texture) {
            t.live = true;
            t.last_used = self.frame_counter;
            return Ok(());
        }
        let format = vk_format(desc.dxgi_name);
        // SAFETY: image + memory + view + descriptor set, all checked, all owned
        // by the entry inserted below and destroyed together.
        unsafe {
            let image = self
                .device
                .create_image(
                    &vk::ImageCreateInfo::default()
                        .image_type(vk::ImageType::TYPE_2D)
                        .format(format)
                        .extent(vk::Extent3D {
                            width: desc.width,
                            height: desc.height,
                            depth: 1,
                        })
                        .mip_levels(desc.mips.max(1))
                        .array_layers(1)
                        .samples(vk::SampleCountFlags::TYPE_1)
                        .tiling(vk::ImageTiling::OPTIMAL)
                        .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
                        .sharing_mode(vk::SharingMode::EXCLUSIVE)
                        .initial_layout(vk::ImageLayout::UNDEFINED),
                    None,
                )
                .map_err(|e| err("create_image", e))?;
            let req = self.device.get_image_memory_requirements(image);
            let idx = self.find_memory(req.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL)?;
            let memory = self
                .device
                .allocate_memory(
                    &vk::MemoryAllocateInfo::default()
                        .allocation_size(req.size)
                        .memory_type_index(idx),
                    None,
                )
                .map_err(|e| err("allocate_memory(image)", e))?;
            self.device
                .bind_image_memory(image, memory, 0)
                .map_err(|e| err("bind_image_memory", e))?;

            let layouts = [self.set_layout];
            let set = self
                .device
                .allocate_descriptor_sets(
                    &vk::DescriptorSetAllocateInfo::default()
                        .descriptor_pool(self.desc_pool)
                        .set_layouts(&layouts),
                )
                .map_err(|e| err("allocate_descriptor_sets", e))?[0];

            // The sampler half never changes; the image half is written when the
            // first mip lands and rewritten when the resident floor moves.
            let sampler_info = [vk::DescriptorImageInfo::default().sampler(self.sampler)];
            self.device.update_descriptor_sets(
                &[vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(1)
                    .descriptor_type(vk::DescriptorType::SAMPLER)
                    .image_info(&sampler_info)],
                &[],
            );

            self.gpu_bytes += req.size;
            self.textures.insert(
                texture,
                VkTexture {
                    image,
                    memory,
                    view: vk::ImageView::null(),
                    set,
                    format,
                    mips: desc.mips.max(1),
                    view_base: u32::MAX,
                    uploaded: 0,
                    bytes: req.size,
                    last_used: self.frame_counter,
                    live: true,
                },
            );
        }
        Ok(())
    }

    fn upload(&mut self, texture: u32, id: SubId, sub: &SubresourceBytes<'_>) -> SimResult<()> {
        let frame = self.frame_counter;
        let len = sub.bytes.len() as u64;
        if self.staging_at + len > self.staging_region {
            // The frame's staging ring is full; the view loop's per-frame upload
            // ceiling normally prevents this, and dropping is better than
            // overrunning the buffer. The subresource is retried next frame.
            return Ok(());
        }
        let Some(t) = self.textures.get_mut(&texture) else {
            return Ok(());
        };
        t.last_used = frame;
        let (image, mip) = (t.image, id.mip);
        t.uploaded |= 1 << mip;

        let offset = self.staging_base + self.staging_at;
        // SAFETY: `staging_ptr` maps `staging_size` bytes and the bounds check
        // above guarantees `offset + len` is inside it.
        unsafe {
            std::ptr::copy_nonoverlapping(
                sub.bytes.as_ptr(),
                self.staging_ptr.add(offset as usize),
                len as usize,
            )
        };
        self.staging_at += len;
        // Keep the next copy's offset legal for block-compressed formats.
        self.staging_at = (self.staging_at + 15) & !15;

        let range = vk::ImageSubresourceRange::default()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .base_mip_level(mip)
            .level_count(1)
            .base_array_layer(0)
            .layer_count(1);
        self.barriers_pre.push(
            vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(image)
                .subresource_range(range)
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE),
        );
        self.copies.push((
            image,
            vk::BufferImageCopy::default()
                .buffer_offset(offset)
                .image_subresource(
                    vk::ImageSubresourceLayers::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .mip_level(mip)
                        .base_array_layer(0)
                        .layer_count(1),
                )
                .image_extent(vk::Extent3D {
                    width: sub.width.max(1),
                    height: sub.height.max(1),
                    depth: 1,
                }),
        ));
        self.barriers_post.push(
            vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(image)
                .subresource_range(range)
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::SHADER_READ),
        );
        Ok(())
    }

    fn set_min_lod(&mut self, texture: u32, min_lod: u32) {
        let Some(t) = self.textures.get(&texture) else {
            return;
        };
        if t.view_base == min_lod || t.uploaded == 0 {
            return;
        }
        let (image, format, mips, old_view, set) = (t.image, t.format, t.mips, t.view, t.set);
        let base = min_lod.min(mips.saturating_sub(1));
        // SAFETY: `begin_frame` has already waited on the in-flight fence, so
        // the GPU is idle with respect to this descriptor set and the view it
        // points at. The replaced view is deferred rather than destroyed, so a
        // caller that skips `begin_frame` still cannot free a live resource.
        unsafe {
            let view = self.device.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(format)
                    .subresource_range(
                        vk::ImageSubresourceRange::default()
                            .aspect_mask(vk::ImageAspectFlags::COLOR)
                            .base_mip_level(base)
                            .level_count(mips - base)
                            .base_array_layer(0)
                            .layer_count(1),
                    ),
                None,
            );
            let Ok(view) = view else { return };
            let info = [vk::DescriptorImageInfo::default()
                .image_view(view)
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
            self.device.update_descriptor_sets(
                &[vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(0)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .image_info(&info)],
                &[],
            );
            if old_view != vk::ImageView::null() {
                // Deferred, not destroyed: a command buffer submitted last frame
                // may still be sampling through this view.
                self.dead_views.push(old_view);
            }
            if let Some(t) = self.textures.get_mut(&texture) {
                t.view = view;
                t.view_base = base;
            }
        }
    }

    fn release_texture(&mut self, texture: u32) {
        if let Some(t) = self.textures.get_mut(&texture) {
            t.live = false;
        }
    }

    fn trim(&mut self, limits: &GpuLimits) -> usize {
        if self.gpu_bytes <= limits.max_gpu_texture_bytes {
            return 0;
        }
        let mut cands: Vec<(u64, u32)> = self
            .textures
            .iter()
            .filter(|(_, t)| !t.live && t.last_used < self.frame_counter)
            .map(|(id, t)| (t.last_used, *id))
            .collect();
        cands.sort_unstable();
        let mut freed = 0;
        for (_, id) in cands.into_iter().take(limits.max_destroys_per_frame) {
            if self.gpu_bytes <= limits.max_gpu_texture_bytes {
                break;
            }
            if let Some(t) = self.textures.remove(&id) {
                self.gpu_bytes = self.gpu_bytes.saturating_sub(t.bytes);
                // Never destroy inline: the GPU may still be reading it. The
                // graveyard is emptied after the next fence wait.
                self.graveyard.push(t);
                freed += 1;
            }
        }
        freed
    }

    fn gpu_bytes(&self) -> u64 {
        self.gpu_bytes
    }

    fn begin_frame(&mut self) -> SimResult<()> {
        // SAFETY: waits until the previous submission has completed, then frees
        // only resources that submission could have referenced. Nothing here is
        // in use by the GPU once the fence is signalled.
        unsafe {
            self.device
                .wait_for_fences(&[self.fence], true, u64::MAX)
                .map_err(|e| err("wait_for_fences", e))?;

            // Safe point: the previous frame has completed, so anything trimmed
            // or replaced during it can now actually be destroyed.
            for t in std::mem::take(&mut self.graveyard) {
                if t.view != vk::ImageView::null() {
                    self.device.destroy_image_view(t.view, None);
                }
                let _ = self.device.free_descriptor_sets(self.desc_pool, &[t.set]);
                self.device.destroy_image(t.image, None);
                self.device.free_memory(t.memory, None);
            }
            for v in std::mem::take(&mut self.dead_views) {
                self.device.destroy_image_view(v, None);
            }

            // Read the previous frame's timestamps before this frame resets them.
            self.last_gpu_ms = f64::NAN;
            if self.have_pending_query {
                let mut ts = [0u64; 2];
                if self
                    .device
                    .get_query_pool_results(
                        self.query_pool,
                        0,
                        &mut ts,
                        vk::QueryResultFlags::TYPE_64,
                    )
                    .is_ok()
                    && ts[1] >= ts[0]
                    && self.timestamp_period > 0.0
                {
                    self.last_gpu_ms =
                        (ts[1] - ts[0]) as f64 * self.timestamp_period as f64 / 1.0e6;
                }
            }
        }
        Ok(())
    }

    fn frame(&mut self, view: &View, visible: &[Quad]) -> SimResult<GpuTimings> {
        self.frame_counter += 1;
        let present_start;
        let gpu_ms = self.last_gpu_ms;

        // SAFETY: single frame in flight. `begin_frame` has already waited on the
        // fence, so every resource touched below is idle with respect to the GPU.
        unsafe {
            let acquired = self.swapchain_fn.acquire_next_image(
                self.swapchain,
                u64::MAX,
                self.sem_acquire,
                vk::Fence::null(),
            );
            let index = match acquired {
                Ok((i, _)) => i,
                // A lost or out-of-date swapchain is not fatal; skip the frame.
                Err(_) => return Ok(GpuTimings::default()),
            };

            self.device
                .reset_fences(&[self.fence])
                .map_err(|e| err("reset_fences", e))?;
            self.device
                .reset_command_buffer(self.cmd, vk::CommandBufferResetFlags::empty())
                .map_err(|e| err("reset_command_buffer", e))?;
            self.device
                .begin_command_buffer(
                    self.cmd,
                    &vk::CommandBufferBeginInfo::default()
                        .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                )
                .map_err(|e| err("begin_command_buffer", e))?;

            self.device
                .cmd_reset_query_pool(self.cmd, self.query_pool, 0, 2);
            self.device.cmd_write_timestamp(
                self.cmd,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                self.query_pool,
                0,
            );

            // Streamed mips first, outside the render pass.
            if !self.copies.is_empty() {
                self.device.cmd_pipeline_barrier(
                    self.cmd,
                    vk::PipelineStageFlags::TOP_OF_PIPE,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &self.barriers_pre,
                );
                for (image, copy) in &self.copies {
                    self.device.cmd_copy_buffer_to_image(
                        self.cmd,
                        self.staging,
                        *image,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        &[*copy],
                    );
                }
                self.device.cmd_pipeline_barrier(
                    self.cmd,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::FRAGMENT_SHADER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &self.barriers_post,
                );
            }
            self.barriers_pre.clear();
            self.copies.clear();
            self.barriers_post.clear();

            let clear = [vk::ClearValue {
                color: vk::ClearColorValue {
                    float32: [0.05, 0.06, 0.08, 1.0],
                },
            }];
            self.device.cmd_begin_render_pass(
                self.cmd,
                &vk::RenderPassBeginInfo::default()
                    .render_pass(self.render_pass)
                    .framebuffer(self.framebuffers[index as usize])
                    .render_area(vk::Rect2D {
                        offset: vk::Offset2D { x: 0, y: 0 },
                        extent: self.extent,
                    })
                    .clear_values(&clear),
                vk::SubpassContents::INLINE,
            );
            self.device
                .cmd_bind_pipeline(self.cmd, vk::PipelineBindPoint::GRAPHICS, self.pipeline);
            self.device
                .cmd_bind_vertex_buffers(self.cmd, 0, &[self.vbuf], &[0]);

            let frame = self.frame_counter;
            for quad in visible {
                let Some(t) = self.textures.get_mut(&quad.texture) else {
                    continue;
                };
                if t.view == vk::ImageView::null() {
                    continue; // nothing resident yet
                }
                t.last_used = frame;
                let set = t.set;
                let mvp = view.view_proj.mul(quad.model).transposed();
                self.device.cmd_push_constants(
                    self.cmd,
                    self.pipeline_layout,
                    vk::ShaderStageFlags::VERTEX,
                    0,
                    std::slice::from_raw_parts(mvp.as_ptr() as *const u8, 64),
                );
                self.device.cmd_bind_descriptor_sets(
                    self.cmd,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.pipeline_layout,
                    0,
                    &[set],
                    &[],
                );
                self.device.cmd_draw(self.cmd, QUAD.len() as u32, 1, 0, 0);
            }

            self.device.cmd_end_render_pass(self.cmd);
            self.device.cmd_write_timestamp(
                self.cmd,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                self.query_pool,
                1,
            );
            self.device
                .end_command_buffer(self.cmd)
                .map_err(|e| err("end_command_buffer", e))?;

            let wait = [self.sem_acquire];
            let signal = [self.sem_render];
            let stages = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
            let cmds = [self.cmd];
            self.device
                .queue_submit(
                    self.queue,
                    &[vk::SubmitInfo::default()
                        .wait_semaphores(&wait)
                        .wait_dst_stage_mask(&stages)
                        .command_buffers(&cmds)
                        .signal_semaphores(&signal)],
                    self.fence,
                )
                .map_err(|e| err("queue_submit", e))?;
            self.have_pending_query = true;

            // Flip regions only after this frame's copies are submitted, so the
            // next frame's memcpys land somewhere the GPU is not reading.
            self.staging_at = 0;
            self.staging_base = if self.staging_base == 0 {
                self.staging_region
            } else {
                0
            };

            present_start = std::time::Instant::now();
            let swapchains = [self.swapchain];
            let indices = [index];
            let _ = self.swapchain_fn.queue_present(
                self.queue,
                &vk::PresentInfoKHR::default()
                    .wait_semaphores(&signal)
                    .swapchains(&swapchains)
                    .image_indices(&indices),
            );
        }

        Ok(GpuTimings {
            gpu_ms,
            present_ms: present_start.elapsed().as_secs_f64() * 1e3,
            // Vulkan reports GPU memory only via VK_EXT_memory_budget, which is
            // not requested here; zero would be a lie, so it stays absent.
            vram_used_mb: f64::NAN,
            vram_budget_mb: f64::NAN,
        })
    }

    fn pump(&mut self) -> bool {
        self.window.pump()
    }

    fn set_caption(&mut self, caption: &str) {
        self.window.set_title(caption);
    }
}

impl Drop for VulkanViewport {
    fn drop(&mut self) {
        // SAFETY: wait for the device to go idle first, so nothing destroyed
        // below is still in use by a submission.
        unsafe {
            let _ = self.device.device_wait_idle();
            for t in self
                .textures
                .drain()
                .map(|(_, t)| t)
                .chain(self.graveyard.drain(..))
            {
                if t.view != vk::ImageView::null() {
                    self.device.destroy_image_view(t.view, None);
                }
                self.device.destroy_image(t.image, None);
                self.device.free_memory(t.memory, None);
            }
            self.device.destroy_query_pool(self.query_pool, None);
            self.device.unmap_memory(self.staging_mem);
            self.device.destroy_buffer(self.staging, None);
            self.device.free_memory(self.staging_mem, None);
            self.device.destroy_buffer(self.vbuf, None);
            self.device.free_memory(self.vmem, None);
            self.device.destroy_semaphore(self.sem_acquire, None);
            self.device.destroy_semaphore(self.sem_render, None);
            self.device.destroy_fence(self.fence, None);
            self.device.destroy_command_pool(self.cmd_pool, None);
            self.device.destroy_sampler(self.sampler, None);
            self.device.destroy_descriptor_pool(self.desc_pool, None);
            self.device
                .destroy_descriptor_set_layout(self.set_layout, None);
            self.device.destroy_pipeline(self.pipeline, None);
            self.device
                .destroy_pipeline_layout(self.pipeline_layout, None);
            for fb in self.framebuffers.drain(..) {
                self.device.destroy_framebuffer(fb, None);
            }
            for v in self.views.drain(..) {
                self.device.destroy_image_view(v, None);
            }
            self.device.destroy_render_pass(self.render_pass, None);
            self.swapchain_fn.destroy_swapchain(self.swapchain, None);
            self.device.destroy_device(None);
            self.surface_fn.destroy_surface(self.surface, None);
            self.instance.destroy_instance(None);
        }
    }
}
