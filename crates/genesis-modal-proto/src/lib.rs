pub mod modal {
    pub mod client {
        tonic::include_proto!("modal.client");
    }
    pub mod task_command_router {
        tonic::include_proto!("modal.task_command_router");
    }
}
